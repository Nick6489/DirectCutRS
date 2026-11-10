use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use freshen::{
    Cancellation, Candidate, Event, Handoff, InstallPolicy, InstallState, PreparedUpdate,
    TrustStore, Updater,
};
use rungui::*;

const EXE: &str = "directcut-rs.exe";
#[cfg(not(feature = "freshen-demo"))]
const PRODUCT: &str = "directcut-rs";
#[cfg(not(feature = "freshen-demo"))]
const PUBLISHER_KEY: &str = "RHo+Y/S/Qfkb+MohAlnxKgAlO3pXKSbZ4J/boByEJF8=";
#[cfg(not(feature = "freshen-demo"))]
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(not(feature = "freshen-demo"))]
pub const CONFIRM_DELAY_MS: u32 = 1;
#[cfg(not(feature = "freshen-demo"))]
type UpdateTransport = freshen::HttpTransport;

#[cfg(feature = "freshen-demo")]
#[path = "updates_demo.rs"]
mod demo;
#[cfg(feature = "freshen-demo")]
pub use demo::{CONFIRM_DELAY_MS, VERSION};
#[cfg(feature = "freshen-demo")]
use demo::{PRODUCT, PUBLISHER_KEY, UpdateTransport, source, transport};

#[cfg(not(feature = "freshen-demo"))]
fn transport() -> freshen::Result<UpdateTransport> {
    freshen::HttpTransport::new(std::time::Duration::from_secs(90))
}

#[cfg(not(feature = "freshen-demo"))]
fn source() -> freshen::ReleaseSource {
    freshen::ReleaseSource::GitHub {
        owner: "Nick6489".into(),
        repository: "DirectCutRS".into(),
    }
}

fn installation_root() -> freshen::Result<PathBuf> {
    let executable = std::env::current_exe()?.canonicalize()?;
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        freshen::Error::Invalid("DirectCut RS installation folder is unknown".into())
    })
}

fn local_policy() -> freshen::Result<InstallPolicy> {
    if !std::env::current_exe()?
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(EXE))
    {
        return Err(freshen::Error::Invalid(format!(
            "The portable executable must be named {EXE} to install an update."
        )));
    }
    Ok(InstallPolicy::portable(
        installation_root()?,
        EXE.into(),
        vec![EXE.into()],
    ))
}

fn updater() -> freshen::Result<Updater<UpdateTransport>> {
    let key = STANDARD
        .decode(PUBLISHER_KEY)
        .map_err(|_| freshen::Error::Signature)?
        .try_into()
        .map_err(|_| freshen::Error::Signature)?;
    Ok(Updater {
        product: PRODUCT.into(),
        channel: "stable".into(),
        current_version: VERSION
            .parse()
            .map_err(|_| freshen::Error::Invalid("DirectCut RS has an invalid version".into()))?,
        target: format!("{}-pc-windows-msvc", std::env::consts::ARCH),
        trust: TrustStore::new(vec![key])?,
        transport: transport()?,
        max_download: 64 * 1024 * 1024,
        max_unpacked: 128 * 1024 * 1024,
    })
}

pub fn confirm_startup(window: Window) {
    if std::env::var_os("FRESHEN_CONFIRM_TOKEN").is_none() {
        return;
    }
    if let Err(error) = installation_root().and_then(|root| freshen::confirm_startup(&root)) {
        message_box(
            Some(window),
            MessageKind::Error,
            Buttons::Ok,
            "Update confirmation",
            &format!(
                "DirectCut RS could not confirm startup. {}",
                update_error(&error)
            ),
        );
    }
}

fn update_error(error: &freshen::Error) -> String {
    match error {
        freshen::Error::Http(error) => match error.status() {
            Some(status) => format!("The update server returned HTTP {}.", status.as_u16()),
            None => "Could not connect to the update server.".into(),
        },
        freshen::Error::UnsafePath(_) => "An update file has an unsafe path.".into(),
        freshen::Error::Helper(_) => "The update helper could not complete its work.".into(),
        _ => error.to_string(),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Progress {
    Idle,
    Busy,
    Fraction(f64),
}

struct Status {
    text: TextInput,
    bar: ProgressBar,
    progress: Cell<Progress>,
}

impl Status {
    fn new(column: VBox) -> Self {
        Label::new(column, "Update status");
        // Win32 multiline edits can consume Tab even when read-only.
        let text = TextInput::new(column);
        text.set_a11y_name("Update status");
        text.set_read_only(true);
        text.set_min_size(460, 24);
        let bar = ProgressBar::new(column);
        bar.set_a11y_name("Update progress");
        Self {
            text,
            bar,
            progress: Cell::new(Progress::Idle),
        }
    }

    fn show(&self, text: &str, progress: Progress) {
        if self.text.text() != text {
            self.text.set_text(text);
        }
        let previous = self.progress.replace(progress);
        if previous == progress {
            return;
        }
        if (previous == Progress::Busy) != (progress == Progress::Busy) {
            self.bar.set_indeterminate(progress == Progress::Busy);
        }
        match progress {
            Progress::Idle => self.bar.set_fraction(0.0),
            Progress::Busy => {}
            Progress::Fraction(fraction) => self.bar.set_fraction(fraction),
        }
    }
}

struct Update {
    updater: Updater<UpdateTransport>,
    candidate: Candidate,
    policy: InstallPolicy,
}

enum Outcome {
    Prepared(Box<PreparedUpdate>, InstallPolicy),
    Armed(Handoff),
}

type StatusMessage = (&'static str, Progress);

struct Job {
    result: Receiver<freshen::Result<Outcome>>,
    progress: Arc<Mutex<Option<StatusMessage>>>,
    cancel: Option<Cancellation>,
}

impl Job {
    fn spawn(
        cancellable: bool,
        run: impl FnOnce(&Cancellation, &mut dyn FnMut(Event)) -> freshen::Result<Outcome>
        + Send
        + 'static,
    ) -> std::io::Result<Self> {
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let progress = Arc::new(Mutex::new(None));
        let progress_tx = progress.clone();
        let (send, result) = mpsc::channel();
        thread::Builder::new()
            .name("DirectCut RS update".into())
            .spawn(move || {
                let outcome = run(&token, &mut |event| {
                    let next = match event {
                        Event::Checking => ("Checking for signed releases...", Progress::Busy),
                        Event::Downloading { received, total } => (
                            "Downloading the update...",
                            match total.filter(|total| *total > 0) {
                                Some(total) => {
                                    Progress::Fraction((received as f64 / total as f64).min(1.0))
                                }
                                None => Progress::Busy,
                            },
                        ),
                        Event::Verifying => ("Verifying the downloaded update...", Progress::Busy),
                        Event::Extracting { .. } => {
                            ("Extracting the verified update...", Progress::Busy)
                        }
                        Event::Ready => ("Update verified.", Progress::Busy),
                        _ => return,
                    };
                    *progress_tx.lock().unwrap() = Some(next);
                });
                let _ = send.send(outcome);
            })?;
        Ok(Self {
            result,
            progress,
            cancel: cancellable.then_some(cancel),
        })
    }
}

fn check() -> freshen::Result<Option<Update>> {
    let policy = local_policy()?;
    let updater = updater()?;
    let candidate = updater.check_for_kind(
        &source(),
        policy.package_kind(),
        &Cancellation::default(),
        &mut |_| {},
    )?;
    Ok(candidate.map(|candidate| Update {
        updater,
        candidate,
        policy,
    }))
}

pub fn display_version(version: &str) -> &str {
    version.strip_suffix(".0").unwrap_or(version)
}

#[derive(Clone, Copy)]
struct About {
    window: Window,
    check: Button,
    status: Label,
}

pub struct Checks {
    launcher: Window,
    launcher_open: Button,
    launcher_about: Button,
    documents: Rc<Cell<usize>>,
    about: Cell<Option<About>>,
    running: Cell<bool>,
    manual: Cell<bool>,
    pub file_picker_open: Cell<bool>,
}

impl Checks {
    pub fn new(
        launcher: Window,
        launcher_open: Button,
        launcher_about: Button,
        documents: Rc<Cell<usize>>,
    ) -> Self {
        Self {
            launcher,
            launcher_open,
            launcher_about,
            documents,
            about: Cell::new(None),
            running: Cell::new(false),
            manual: Cell::new(false),
            file_picker_open: Cell::new(false),
        }
    }

    pub fn about(self: &Rc<Self>) {
        let window = Window::new("About DirectCut RS");
        let column = VBox::new(window);
        column.set_padding(16);
        column.set_spacing(8);
        Label::new(
            column,
            &format!("DirectCut RS {}", display_version(VERSION)),
        );
        Label::new(column, "By Nick Giannak III");
        Label::new(column, "Contributions by Derek Lane.");
        Label::new(column, "Made exclusively with artificial intelligence.");
        Label::new(column, "Copyright © 2026 Nick Giannak III");
        Label::new(column, "Released under the MIT License.");
        let status = Label::new(column, "");
        let row = HBox::new(column);
        row.set_spacing(8);
        let check = Button::new(row, "&Check for updates");
        let close = Button::new(row, "&Close");
        check.on_click({
            let checks = self.clone();
            move || {
                status.set_text("Checking for updates...");
                checks.start(true);
            }
        });
        close.on_click(move || window.hide());
        window.on_cancel(move || window.hide());
        self.about.set(Some(About {
            window,
            check,
            status,
        }));
        window.set_size(435, 275);
        Timer::once(1, move || check.focus());
        window.run_modal(Some(self.launcher));
        self.about.set(None);
        self.manual.set(false);
        if window.is_alive() {
            window.destroy();
        }
        if self.launcher_about.is_alive() {
            self.launcher_about.focus();
        }
    }

    pub fn start(self: &Rc<Self>, manual: bool) {
        if manual {
            self.manual.set(true);
        }
        if self.running.replace(true) {
            return;
        }
        let (send, receive) = mpsc::channel();
        if let Err(error) = thread::Builder::new()
            .name("DirectCut RS release check".into())
            .spawn(move || {
                let _ = send.send(check());
            })
        {
            self.complete(Err(error.into()));
            return;
        }
        let timer = Rc::new(Cell::new(None::<Timer>));
        let handle = Timer::every(100, {
            let checks = self.clone();
            let timer = timer.clone();
            move || {
                if checks.file_picker_open.get() {
                    return;
                }
                let result = match receive.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        Err(freshen::Error::Helper("update check stopped".into()))
                    }
                };
                if let Some(handle) = timer.take() {
                    handle.stop();
                }
                checks.complete(result);
            }
        });
        timer.set(Some(handle));
    }

    fn complete(&self, result: freshen::Result<Option<Update>>) {
        self.running.set(false);
        let manual = self.manual.replace(false);
        let about = self.about.get().filter(|about| about.window.is_alive());
        if let Some(about) = about {
            about.status.set_text("");
        }
        if !self.launcher.is_alive() {
            return;
        }
        match result {
            Ok(Some(update)) => {
                let (parent, focus) = about.map_or((self.launcher, self.launcher_open), |about| {
                    (about.window, about.check)
                });
                show(parent, focus, self.documents.clone(), update);
            }
            result if manual => {
                if let Some(about) = about {
                    let (kind, text) = match result {
                        Ok(_) => (
                            MessageKind::Info,
                            "You have the latest available version.".into(),
                        ),
                        Err(error) => (
                            MessageKind::Error,
                            format!("Could not check for updates. {}", update_error(&error)),
                        ),
                    };
                    message_box(
                        Some(about.window),
                        kind,
                        Buttons::Ok,
                        "DirectCut RS updates",
                        &text,
                    );
                    about.check.focus();
                }
            }
            _ => {}
        }
    }
}

fn arm(prepared: PreparedUpdate, policy: InstallPolicy) -> freshen::Result<Outcome> {
    if let Some(status) = freshen::installation_status(&policy.root)? {
        match status.state {
            InstallState::Prepared | InstallState::Committed | InstallState::RolledBack => {
                // Windows cannot remove a mapped helper image. Check it before
                // Freshen starts removing the previous transaction's files.
                let image = policy.root.join(".freshen/transaction/helper.exe");
                if let Err(error) = std::fs::OpenOptions::new().write(true).open(image)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(freshen::Error::Recovery(format!(
                        "Close the previous update helper before retrying. {error}"
                    )));
                }
                freshen::discard_finished_installation(&policy.root)?;
            }
            _ => {
                return Err(freshen::Error::Recovery(format!(
                    "The previous update ({}) needs recovery. Close DirectCut RS before recovering the installation.",
                    status.version
                )));
            }
        }
    }
    Ok(Outcome::Armed(
        prepared.arm_current_exe(policy, Vec::new())?,
    ))
}

struct Dialog {
    window: Window,
    status: Status,
    primary: Button,
    close: Button,
    documents: Rc<Cell<usize>>,
    update: Arc<Update>,
    job: Option<Job>,
    cancelling: bool,
}

impl Dialog {
    fn begin(&mut self, job: std::io::Result<Job>, text: &str) {
        match job {
            Ok(job) => {
                let cancellable = job.cancel.is_some();
                self.job = Some(job);
                self.status.show(text, Progress::Busy);
                self.close
                    .set_text(if cancellable { "&Cancel" } else { "&Close" });
                self.close.set_enabled(cancellable);
                if cancellable {
                    self.close.focus();
                } else {
                    self.status.text.focus();
                }
                self.primary.set_enabled(false);
            }
            Err(error) => self.finish(&format!("Could not start the update operation. {error}")),
        }
    }

    fn finish(&mut self, message: &str) {
        self.status.show(message, Progress::Idle);
        self.primary.set_enabled(true);
        self.close.set_text("&Close");
        self.close.set_enabled(true);
        self.primary.focus();
        self.cancelling = false;
    }

    fn primary(&mut self) {
        if self.job.is_some() {
            return;
        }
        if self.documents.get() > 0 {
            self.status.show(
                "Close all MP3 editor windows before installing. Save any edits first.",
                Progress::Idle,
            );
            return;
        }
        let update = self.update.clone();
        let size = update.candidate.artifact().size;
        self.begin(
            Job::spawn(true, move |cancel, events| {
                let prepared = update.updater.prepare(
                    update.candidate.clone(),
                    &crate::paths::update_work(),
                    cancel,
                    &mut |event| {
                        events(match event {
                            Event::Downloading { received, .. } => Event::Downloading {
                                received,
                                total: Some(size),
                            },
                            event => event,
                        });
                    },
                )?;
                Ok(Outcome::Prepared(Box::new(prepared), update.policy.clone()))
            }),
            "Downloading the update...",
        );
    }

    // A running check/download can be cancelled. A handoff waits for a verified,
    // ready helper before the application exits.
    fn request_close(&mut self) -> bool {
        let Some(job) = &self.job else { return true };
        if let Some(cancel) = &job.cancel {
            cancel.cancel();
            self.cancelling = true;
            self.status
                .show("Cancelling the update operation...", Progress::Busy);
            self.status.text.focus();
            self.close.set_enabled(false);
        }
        false
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        if !self.cancelling
            && let Some((text, progress)) = job.progress.lock().unwrap().take()
        {
            self.status.show(text, progress);
        }
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err(freshen::Error::Helper("update worker stopped".into()))
            }
        };
        self.job = None;
        if self.cancelling {
            self.finish("Update operation cancelled.");
            return;
        }
        match result {
            Ok(Outcome::Prepared(prepared, policy)) => {
                self.begin(
                    Job::spawn(false, move |_, _| arm(*prepared, policy)),
                    "Starting the update helper...",
                );
            }
            Ok(Outcome::Armed(handoff)) => {
                self.status.show(
                    "Update helper is ready. Restarting DirectCut RS...",
                    Progress::Busy,
                );
                // Dropping a ready handoff deliberately leaves the helper armed.
                drop(handoff);
                App::quit();
            }
            Err(error) => self.finish(&format!("Update failed. {}", update_error(&error))),
        }
    }
}

fn show(parent: Window, return_focus: Button, documents: Rc<Cell<usize>>, update: Update) {
    let window = Window::new("DirectCut RS updates");
    let column = VBox::new(window);
    column.set_padding(14);
    column.set_spacing(8);
    Label::new(
        column,
        &format!("Installed version {}", display_version(VERSION)),
    );
    let available = update.candidate.release().version.to_string();
    Label::new(
        column,
        &format!("DirectCut RS {} is available.", display_version(&available)),
    );
    let status = Status::new(column);
    status.show(
        "Choose Install update to download and restart DirectCut RS.",
        Progress::Idle,
    );
    let row = HBox::new(column);
    row.set_spacing(8);
    let primary = Button::new(row, "&Install update");
    let close = Button::new(row, "&Close");
    let dialog = Rc::new(RefCell::new(Dialog {
        window,
        status,
        primary,
        close,
        documents,
        update: Arc::new(update),
        job: None,
        cancelling: false,
    }));
    primary.on_click({
        let dialog = dialog.clone();
        move || dialog.borrow_mut().primary()
    });
    let close_request = {
        let dialog = dialog.clone();
        move || {
            let mut dialog = dialog.borrow_mut();
            if dialog.request_close() {
                dialog.window.hide();
            }
        }
    };
    close.on_click(close_request.clone());
    window.on_cancel(close_request);
    window.on_close({
        let dialog = dialog.clone();
        move || dialog.borrow_mut().request_close()
    });
    let timer = Timer::every(100, move || dialog.borrow_mut().poll());
    window.set_size(515, 240);
    // Defer focus until the modal window is visible.
    Timer::once(1, move || primary.focus());
    window.run_modal(Some(parent));
    timer.stop();
    if window.is_alive() {
        window.destroy();
    }
    if return_focus.is_alive() {
        return_focus.focus();
    }
}

fn helper_root(args: &[OsString]) -> Option<&Path> {
    let [_, plan, _] = args else { return None };
    let plan = Path::new(plan);
    if !plan.ends_with(".freshen/transaction/plan.json") {
        return None;
    }
    plan.ancestors().nth(3)
}

fn helper_phase(state: InstallState) -> &'static str {
    match state {
        InstallState::Prepared => "Waiting for DirectCut RS to close...",
        InstallState::Applying => "Replacing the DirectCut RS executable...",
        InstallState::AwaitingConfirmation => {
            "Waiting for the new DirectCut RS to confirm startup..."
        }
        InstallState::RollingBack => "Restoring the previous version...",
        _ => "Finalizing the update...",
    }
}

pub fn run_helper(args: Vec<OsString>) -> ExitCode {
    let Some(root) = helper_root(&args).map(Path::to_path_buf) else {
        return ExitCode::FAILURE;
    };
    let Ok(app) = App::new("DirectCut RS Update Helper") else {
        return if freshen::run_helper(args).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    };

    let window = Window::new("Installing DirectCut RS update");
    let column = VBox::new(window);
    column.set_padding(14);
    column.set_spacing(8);
    let status = Status::new(column);
    let status_text = status.text;
    status.show("Preparing the update helper...", Progress::Busy);
    let close = Button::new(column, "&Close");
    close.set_enabled(false);
    let finished = Rc::new(Cell::new(None::<ExitCode>));
    window.on_close({
        let finished = finished.clone();
        move || finished.get().is_some()
    });
    close.on_click(move || window.close());
    window.on_cancel({
        let finished = finished.clone();
        move || {
            if finished.get().is_some() {
                window.close();
            }
        }
    });

    let (send, result) = mpsc::channel();
    if thread::Builder::new()
        .name("DirectCut RS installer".into())
        .spawn(move || {
            let _ = send.send(freshen::run_helper(args));
        })
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let timer = Timer::every(100, {
        let finished = finished.clone();
        move || {
            if finished.get().is_some() {
                return;
            }
            let outcome = match result.try_recv() {
                Err(TryRecvError::Empty) => {
                    if let Ok(Some(state)) = freshen::installation_status(&root) {
                        status.show(helper_phase(state.state), Progress::Busy);
                    }
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    Err(freshen::Error::Helper("installer thread stopped".into()))
                }
                Ok(outcome) => outcome,
            };
            let exit = if matches!(&outcome, Ok(true)) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
            finished.set(Some(exit));
            match freshen::installation_status(&root)
                .ok()
                .flatten()
                .map(|status| status.state)
            {
                Some(InstallState::Committed) if matches!(outcome, Ok(true)) => {
                    status.show(
                        "Update complete. DirectCut RS restarted successfully.",
                        Progress::Fraction(1.0),
                    );
                }
                Some(InstallState::Prepared) | None => {
                    // Cancellation and readiness errors must let the waiting parent
                    // observe process exit. Freshen records the readiness diagnostic.
                    App::quit();
                    return;
                }
                state => {
                    let prefix = if state == Some(InstallState::RolledBack) {
                        "Update failed. The previous version was restored."
                    } else {
                        "Update did not complete. Recovery information is available."
                    };
                    let text = match outcome {
                        Err(error) => format!("{prefix} {}", update_error(&error)),
                        Ok(_) => prefix.into(),
                    };
                    status.show(&text, Progress::Idle);
                }
            }
            close.set_enabled(true);
            close.focus();
        }
    });
    window.set_size(515, 200);
    window.show();
    status_text.focus();
    app.run();
    timer.stop();
    finished.get().unwrap_or(ExitCode::FAILURE)
}
