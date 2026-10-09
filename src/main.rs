#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod catalog;
mod operation_progress;
mod ui;
use catalog::Choice;
use gpui::{
    App, Application, Bounds, Context, Div, Image, ImageFormat, PathPromptOptions, ScrollHandle,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, img, prelude::*, px, rgb, size,
};
use operation_progress::OperationProgress;
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use xppen_firmware_core::{
    Result,
    image::{self, Firmware},
    native::NativeDevice,
    progress::{self, Progress, Stage},
    utility::{Mode, Runner},
    workflow::{self, DeviceLock},
};

const BACKGROUND: u32 = 0x233d4d;
const ACCENT: u32 = 0x38a3a5;
const MUTED: u32 = 0xb4ccd5;

#[derive(Clone)]
enum Operation {
    Identify,
    Backup,
    Install {
        firmware: Firmware,
        backup: Vec<u8>,
        recovery: bool,
    },
    Leave,
}

struct Completed {
    mode: Mode,
    image: Option<Vec<u8>>,
    fresh_backup: bool,
    message: String,
    session: PathBuf,
}

enum Event {
    Output(String),
    Progress(Progress),
    Complete(Result<Completed>),
}

/// Windows keeps portable backups beside its executable. Unix uses user data
/// directories so macOS bundles/signatures and system installs remain untouched.
fn app_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME is required"))
                    .join(".local/share")
            })
            .join("xppen-firmware")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(std::env::var_os("HOME").expect("HOME is required"))
            .join("Library/Application Support/xppen-firmware")
    }
}

fn perform(root: &Path, operation: Operation, events: &mpsc::Sender<Event>) -> Result<Completed> {
    let _ = events.send(Event::Progress(Progress::Started(Stage::Prepare)));
    let _lock = DeviceLock::acquire(&root.join("backups"))?;
    let session = workflow::new_session(&root.join("backups"))?;
    let _ = events.send(Event::Output(format!("Session: {}\n", session.display())));
    let mut runner = NativeDevice::new(
        &session.join("commands.log"),
        |text| {
            let _ = events.send(Event::Output(text.to_string()));
        },
        |event| {
            let _ = events.send(Event::Progress(event));
        },
    )?;
    runner.progress(Progress::Finished(Stage::Prepare));
    let result = match operation {
        Operation::Identify => workflow::connect(&mut runner).map(|mode| Completed { mode, image: None, fresh_backup: false, message: format!("Connected: {}", if mode == Mode::Runtime { "Deco Fun XS in normal mode" } else { "IC902 / FFF1 bootloader" }), session: session.clone() }),
        Operation::Backup => workflow::backup(&mut runner, &session).map(|main| Completed { mode: Mode::Runtime, image: Some(main), fresh_backup: true, message: "Full backup saved and verified with a double read. Tablet returned to normal mode.".into(), session: session.clone() }),
        Operation::Install { firmware, backup, recovery } => workflow::install(&mut runner, &firmware, &backup, &session, recovery).map(|main| Completed { mode: Mode::Runtime, image: Some(main), fresh_backup: false, message: format!("{} installed. All 16 flash block CRCs matched; tablet returned to normal mode.", firmware.name), session: session.clone() }),
        Operation::Leave => workflow::leave(&mut runner).map(|()| Completed { mode: Mode::Runtime, image: None, fresh_backup: false, message: "Tablet returned to normal mode.".into(), session: session.clone() }),
    };
    let summary = match &result {
        Ok(done) => format!("SUCCESS: {}", done.message),
        Err(error) => format!("FAILED: {error}"),
    };
    progress::step(&mut runner, Stage::SaveLog, |_| {
        workflow::save_new(&session.join("result.txt"), summary.as_bytes())
    })?;
    result.map_err(|error| format!("{error}\nLogs: {}", session.display()))
}

struct Flasher {
    root: PathBuf,
    busy: Arc<AtomicBool>,
    connected: Option<Mode>,
    firmware: Firmware,
    selected: Option<Choice>,
    tablet: Arc<Image>,
    show_tools: bool,
    progress: Option<OperationProgress>,
    scroll: ScrollHandle,
    current_backup: Option<Vec<u8>>,
    saved_backup: Option<Vec<u8>>,
    backup_label: String,
    status: String,
    error: bool,
    output: String,
    session: Option<PathBuf>,
    confirmation: Option<Operation>,
    risk_acknowledged: bool,
    recovery_required: bool,
}

impl Flasher {
    fn new(cx: &mut Context<Self>) -> Self {
        // A startup fixture exercises rendering without opening the tablet.
        let fixture = std::env::args().any(|arg| arg.starts_with("--preview-confirmation"));
        let firmware = Choice::Fast
            .firmware()
            .expect("Embedded optimized firmware must match its pin");
        let mut this = Self {
            root: app_root(),
            busy: Arc::new(AtomicBool::new(false)),
            connected: None,
            firmware,
            selected: Some(Choice::Fast),
            tablet: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../assets/tablet.png").to_vec(),
            )),
            show_tools: false,
            progress: None,
            scroll: ScrollHandle::new(),
            current_backup: None,
            saved_backup: None,
            backup_label: "No backup loaded".into(),
            status: "Close OpenTabletDriver and other tablet software before continuing.".into(),
            error: false,
            output: String::new(),
            session: None,
            confirmation: None,
            risk_acknowledged: false,
            recovery_required: false,
        };
        if fixture {
            this.confirmation = Some(Operation::Install {
                firmware: this.firmware.clone(),
                backup: Vec::new(),
                recovery: std::env::args().any(|arg| arg == "--preview-confirmation-restore"),
            });
            this.risk_acknowledged =
                std::env::args().any(|arg| arg == "--preview-confirmation-acknowledged");
            this.status = "Preview only: no device access.".into();
            if std::env::args().any(|arg| arg == "--smoke-test") {
                // Exercise the actual rejection handler with USB independently
                // suppressed by preview flags, even if this assertion fails.
                let acknowledged = this.risk_acknowledged;
                this.risk_acknowledged = false;
                this.flash_confirmed(cx);
                assert!(
                    this.confirmation.is_some(),
                    "Unacknowledged flash consumed confirmation"
                );
                this.risk_acknowledged = true;
                this.busy.store(true, Ordering::SeqCst);
                this.flash_confirmed(cx);
                assert!(
                    this.confirmation.is_some(),
                    "Busy flash consumed confirmation"
                );
                this.busy.store(false, Ordering::SeqCst);
                this.risk_acknowledged = acknowledged;
            }
        }
        if let Some(preview) = std::env::args().find(|arg| arg.starts_with("--preview-progress")) {
            let mut progress = OperationProgress::new(&if preview == "--preview-progress-flash" {
                Operation::Install {
                    firmware: this.firmware.clone(),
                    backup: Vec::new(),
                    recovery: false,
                }
            } else {
                Operation::Backup
            });
            for stage in [Stage::Prepare, Stage::EnterDfu] {
                progress.apply(Progress::Started(stage));
                progress.apply(Progress::Finished(stage));
            }
            progress.apply(Progress::Started(Stage::ReadBackup));
            if preview == "--preview-progress-wait" {
                progress.steps[2].state = operation_progress::State::Pending;
                progress.steps[1].state = operation_progress::State::Active;
            } else {
                progress.apply(Progress::Advanced {
                    stage: Stage::ReadBackup,
                    completed: 32768,
                    total: 65536,
                });
            }
            if preview == "--preview-progress-failed" {
                progress.apply(Progress::Failed(Stage::ReadBackup));
                progress.apply(Progress::Started(Stage::ReturnRuntime));
                progress.apply(Progress::Finished(Stage::ReturnRuntime));
                progress.finish(false);
                this.error = true;
            }
            if preview == "--preview-progress-flash" {
                for stage in [Stage::SaveRollback, Stage::CheckCurrent] {
                    progress.apply(Progress::Started(stage));
                    progress.apply(Progress::Finished(stage));
                }
                progress.apply(Progress::Started(Stage::WriteFirmware));
                progress.apply(Progress::Advanced {
                    stage: Stage::WriteFirmware,
                    completed: 946,
                    total: 1893,
                });
            }
            this.progress = Some(progress);
            this.status = "Preview only: no device access.".into();
        }
        cx.notify();
        this
    }

    fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn message(&mut self, result: Result<String>, cx: &mut Context<Self>) {
        self.error = result.is_err();
        self.status = result.unwrap_or_else(|error| error);
        cx.notify();
    }

    /// Poll only UI events. USB work and utility output stay on a worker thread.
    fn start(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.is_busy()
            || std::env::args().any(|arg| {
                arg.starts_with("--preview-progress")
                    || arg.starts_with("--preview-confirmation")
                    || matches!(arg.as_str(), "--preview-details" | "--smoke-test")
            })
        {
            return;
        }
        self.busy.store(true, Ordering::SeqCst);
        self.error = false;
        self.confirmation = None;
        self.risk_acknowledged = false;
        self.status = "Working… Keep the tablet connected.".into();
        self.output.clear();
        self.progress = Some(OperationProgress::new(&operation));
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        let installing = matches!(operation, Operation::Install { .. });
        let (tx, rx) = mpsc::channel();
        let root = self.root.clone();
        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| perform(&root, operation, &tx)))
                .unwrap_or_else(|_| Err("Tablet worker stopped unexpectedly. Retain the operation backup and inspect device mode before continuing.".into()));
            let _ = tx.send(Event::Complete(result));
        });
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let events: Vec<_> = rx.try_iter().collect();
                if events.is_empty() {
                    continue;
                }
                let mut done = false;
                if view
                    .update(cx, |this, cx| {
                        for event in events {
                            match event {
                                Event::Progress(event) => {
                                    if let Some(progress) = &mut this.progress {
                                        progress.apply(event);
                                    }
                                }
                                Event::Output(text) => {
                                    this.output.push_str(&text.replace('\r', "\n"));
                                    if this.output.len() > 24000 {
                                        let mut start = this.output.len() - 20000;
                                        while !this.output.is_char_boundary(start) {
                                            start += 1;
                                        }
                                        this.output.drain(..start);
                                    }
                                }
                                Event::Complete(result) => {
                                    done = true;
                                    this.busy.store(false, Ordering::SeqCst);
                                    if let Some(progress) = &mut this.progress {
                                        progress.finish(result.is_ok());
                                    }
                                    match result {
                                        Ok(completed) => {
                                            if installing {
                                                this.recovery_required = false;
                                            }
                                            this.connected = Some(completed.mode);
                                            this.error = false;
                                            this.status = completed.message;
                                            this.session = Some(completed.session.clone());
                                            if let Some(main) = completed.image {
                                                if completed.fresh_backup {
                                                    this.saved_backup = Some(main.clone());
                                                    this.backup_label = completed
                                                        .session
                                                        .join("verified-full-backup.bin")
                                                        .display()
                                                        .to_string();
                                                }
                                                this.current_backup = Some(main);
                                            }
                                        }
                                        Err(error) => {
                                            if installing {
                                                this.recovery_required = true;
                                            }
                                            this.connected = None;
                                            this.error = true;
                                            this.status = error;
                                        }
                                    }
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                if done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn choose_file(&mut self, backup: bool, cx: &mut Context<Self>) {
        if self.is_busy() || self.confirmation.is_some() {
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                if backup {
                    "Choose full 64 KiB backup (.bin)"
                } else {
                    "Choose application firmware (.hex / .bin)"
                }
                .into(),
            ),
        });
        cx.spawn(async move |view, cx| {
            match selection.await {
                Ok(Ok(Some(paths))) => if let Some(path) = paths.first() {
                    let _ = view.update(cx, |this, cx| {
                        if backup {
                            let result = (|| {
                                if fs::metadata(path).map_err(|e| e.to_string())?.len() != image::FLASH_SIZE as u64 { return Err("Choose a full 64 KiB backup, not application firmware".into()); }
                                let main = image::normalized_backup(&fs::read(path).map_err(|e| e.to_string())?)?;
                                this.current_backup = Some(main.clone()); this.saved_backup = Some(main);
                                this.backup_label = path.display().to_string();
                                Ok("Backup loaded. A live match is required before normal flashing.".into())
                            })();
                            this.message(result, cx);
                        } else {
                            let result = Firmware::load(path).map(|firmware| { this.firmware = firmware; this.selected = None; "Firmware validated. Nothing has been flashed.".into() });
                            this.message(result, cx);
                        }
                    });
                },
                Ok(Err(error)) => { let _ = view.update(cx, |this, cx| this.message(Err(error.to_string()), cx)); },
                _ => {},
            }
        }).detach();
    }

    fn confirm(&mut self, restore: bool, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let backup = if restore {
            &self.saved_backup
        } else {
            &self.current_backup
        };
        let Some(backup) = backup else {
            self.message(Err("Create or load a full backup first.".into()), cx);
            return;
        };
        let firmware = if restore {
            match image::backup_firmware(backup) {
                Ok(firmware) => firmware,
                Err(error) => {
                    self.message(Err(error), cx);
                    return;
                }
            }
        } else {
            self.firmware.clone()
        };
        // Acknowledgement belongs to this particular pending image; never carry
        // consent from a previous flash, restore, or cancelled confirmation.
        self.risk_acknowledged = false;
        self.confirmation = Some(Operation::Install {
            firmware,
            backup: backup.clone(),
            recovery: restore,
        });
        cx.notify();
    }

    fn flash_confirmed(&mut self, cx: &mut Context<Self>) {
        // Enforce this in the handler too, not just by disabling its button.
        if !self.risk_acknowledged || self.is_busy() {
            return;
        }
        if let Some(operation) = self.confirmation.take() {
            self.risk_acknowledged = false;
            self.start(operation, cx);
        }
    }
}

fn main() {
    // Packaging checks the embedded build version without opening a window or
    // accessing USB, so an older executable cannot be mislabeled as a new release.
    if std::env::args().any(|arg| arg == "--version") {
        println!("xppen-firmware {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let smoke_test = std::env::args().any(|arg| arg == "--smoke-test");
    Application::new().run(move |cx: &mut App| {
        // Embedded fonts provide consistent typography across platforms.
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/Lato-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../assets/Lato-Bold.ttf")),
            ])
            .expect("Unable to load embedded Lato fonts");
        let bounds = Bounds::centered(None, size(px(900.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(720.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(
                        format!(
                            "XPPen Firmware v{} — Deco Fun XS",
                            env!("CARGO_PKG_VERSION")
                        )
                        .into(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| cx.new(Flasher::new),
        )
        .expect("Unable to open desktop window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if smoke_test {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }
        cx.activate(true);
    });
}
