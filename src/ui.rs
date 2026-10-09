use super::*;
use gpui::AnimationExt as _;
use operation_progress::State;

fn button(id: &'static str, label: &'static str, enabled: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(if enabled { 0x63818e } else { 0x46616e }))
        .text_color(rgb(if enabled { 0xffffff } else { 0x819ba6 }))
        .text_sm()
        .font_weight(gpui::FontWeight::BOLD)
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|el| el.bg(rgb(0x2c5060)).border_color(rgb(ACCENT)))
        })
        .child(label)
}

fn primary_button(id: &'static str, label: &'static str, enabled: bool) -> gpui::Stateful<Div> {
    button(id, label, enabled).when(enabled, |el| {
        el.bg(rgb(ACCENT))
            .border_color(rgb(ACCENT))
            .hover(|el| el.bg(rgb(0x318f92)).border_color(rgb(0x318f92)))
    })
}

/// Firmware acquisition details displayed in each selection tooltip.
struct FirmwareDetails(Choice);

impl Render for FirmwareDetails {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(390.))
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(ACCENT))
            .bg(rgb(0x1b303d))
            .text_color(rgb(0xffffff))
            .font_family("Lato")
            .flex()
            .flex_col()
            .gap_3()
            .text_sm()
            .shadow_lg()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(self.0.label()),
            )
            .children(
                self.0
                    .details()
                    .iter()
                    .map(|line| div().text_color(rgb(MUTED)).child(*line)),
            )
    }
}

impl Flasher {
    fn progress_panel(&self, progress: &OperationProgress, cx: &mut Context<Self>) -> Div {
        let completed = progress
            .steps
            .iter()
            .filter(|step| step.state == State::Complete)
            .count();
        div()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x1d3341))
            .border_1()
            .border_color(rgb(0x466775))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                        "{}{}",
                        progress.title,
                        if progress.stopped {
                            if progress.succeeded {
                                " · Complete"
                            } else {
                                " · Failed"
                            }
                        } else {
                            ""
                        }
                    )))
                    .when(progress.stopped, |el| {
                        el.child(
                            button("dismiss-progress", "Dismiss", true).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.progress = None;
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(!progress.stopped, |el| {
                        el.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                            "{completed}/{} steps complete",
                            progress.steps.len()
                        )))
                    }),
            )
            .children(progress.steps.iter().enumerate().map(|(index, step)| {
                let color = match step.state {
                    State::Failed => 0xe78d95,
                    State::Pending => 0x7693a1,
                    _ => ACCENT,
                };
                let waiting = step.state == State::Active && step.work.is_none();
                let bar = div()
                    .relative()
                    .w_full()
                    .h(px(5.))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(rgb(0x375767))
                    .when(waiting, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .w(gpui::relative(0.3))
                                .bg(rgb(ACCENT))
                                .rounded_full()
                                .with_animation(
                                    ("step-wait", index),
                                    gpui::Animation::new(Duration::from_millis(1400)).repeat(),
                                    |bar, delta| bar.left(gpui::relative(delta * 0.7)),
                                ),
                        )
                    })
                    .when(!waiting, |el| {
                        el.child(
                            div()
                                .h_full()
                                .w(gpui::relative(step.fraction()))
                                .bg(rgb(color))
                                .rounded_full(),
                        )
                    });
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(if step.state == State::Pending {
                                        MUTED
                                    } else {
                                        0xffffff
                                    }))
                                    .child(format!(
                                        "{} {}",
                                        match step.state {
                                            State::Complete => "✓".into(),
                                            State::Failed => "×".into(),
                                            _ => format!("{}.", index + 1),
                                        },
                                        step.stage.label()
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if step.state == State::Failed {
                                        color
                                    } else {
                                        MUTED
                                    }))
                                    .child(step.detail(progress.stopped)),
                            ),
                    )
                    .child(bar)
            }))
            .when(progress.stopped && !progress.succeeded, |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xffbec2))
                        .child(self.status.clone()),
                )
            })
    }

    fn choice_row(&self, choice: Choice, enabled: bool, cx: &mut Context<Self>) -> Div {
        let selected = self.selected == Some(choice);
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id(choice.id())
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px_4()
                    .py_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BACKGROUND }))
                    .when(selected, |el| el.bg(rgb(0x294c5c)))
                    .when(enabled, |el| {
                        el.cursor_pointer().hover(|el| el.bg(rgb(0x294c5c)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if enabled {
                            let result = choice.firmware().map(|firmware| {
                                this.firmware = firmware;
                                this.selected = Some(choice);
                                format!("{} selected.", choice.label())
                            });
                            this.message(result, cx);
                        }
                    }))
                    .child(
                        div()
                            .w(px(21.))
                            .h(px(21.))
                            .flex_none()
                            .rounded_sm()
                            .border_2()
                            .border_color(rgb(if selected { ACCENT } else { 0x91abb7 }))
                            .bg(rgb(if selected { ACCENT } else { BACKGROUND }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .when(selected, |el| el.child("✓")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(if enabled { 0xffffff } else { MUTED }))
                                    .child(choice.label()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(choice.subtitle()),
                            ),
                    ),
            )
            .child(
                div()
                    .id(gpui::SharedString::from(format!("info-{}", choice.id())))
                    .w(px(25.))
                    .h(px(25.))
                    .flex_none()
                    .mr_2()
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(0x7896a4))
                    .text_color(rgb(MUTED))
                    .text_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_default()
                    .hover(|el| el.text_color(rgb(0xffffff)).border_color(rgb(ACCENT)))
                    .tooltip(move |_, cx| cx.new(|_| FirmwareDetails(choice)).into())
                    .child("i"),
            )
    }
}

impl Render for Flasher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy.clone();
        window.on_window_should_close(cx, move |_, _| !busy.load(Ordering::SeqCst));
        let enabled = !self.is_busy() && self.confirmation.is_none();
        let normal_operations = enabled && !self.recovery_required;
        let can_flash = normal_operations && self.current_backup.is_some();
        let can_restore = enabled && self.saved_backup.is_some();
        let mode = match self.connected {
            Some(Mode::Runtime) => "Tablet connected",
            Some(Mode::Dfu) => "Tablet in DFU mode",
            None => "Tablet not checked",
        };
        let backup_name = Path::new(&self.backup_label)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.backup_label.clone());
        let mut content = div().max_w(px(800.)).mx_auto().flex().flex_col().gap_5()
            .child(div().flex().items_center().gap_5()
                .child(img(self.tablet.clone()).w(px(120.)).h(px(95.)).object_fit(gpui::ObjectFit::Contain))
                .child(div().flex_1().flex().flex_col().gap_1()
                    .child(div().text_size(px(30.)).font_weight(gpui::FontWeight::BOLD).child("XPPen Deco Fun XS"))
                    .child(div().text_size(px(24.)).child("Firmware Flasher"))))
            .when_some(self.progress.as_ref(), |el, progress| el.child(self.progress_panel(progress, cx)))
            .child(div().flex().items_center().justify_between()
                .child(div().text_sm().text_color(rgb(MUTED)).child(mode))
                .child(button("connect", "Check connection", enabled)
                    .on_click(cx.listener(move |this, _, _, cx| { if enabled { this.start(Operation::Identify, cx); } }))))
            .child(div().flex().flex_col().gap_1()
                .children(Choice::ALL.into_iter().map(|choice| self.choice_row(choice, enabled, cx)))
                .when(self.selected.is_none(), |el| el.child(div().px_4().pt_2().text_sm()
                    .text_color(rgb(MUTED)).child(format!("Custom file: {}", self.firmware.name)))))
            .child(div().flex().flex_col().gap_2()
                .child(div().flex().gap_3()
                    .child(button("backup", "Create a backup", normal_operations)
                        .on_click(cx.listener(move |this, _, _, cx| { if normal_operations { this.start(Operation::Backup, cx); } })))
                    .child(button("load", "Load backup", enabled)
                        .on_click(cx.listener(move |this, _, _, cx| { if enabled { this.choose_file(true, cx); } }))))
                .child(div().text_xs().text_color(rgb(MUTED)).child("Creates a verified firmware backup for restoration. This operation takes several minutes."))
                .child(div().id("backup-name").text_xs().text_color(rgb(MUTED)).child(backup_name)
                    .tooltip({ let path = self.backup_label.clone(); move |_, cx| cx.new(|_| BackupDetails(path.clone())).into() })))
            .child(div().flex().gap_3()
                .child(primary_button("flash", "Flash selected firmware", can_flash)
                    .on_click(cx.listener(move |this, _, _, cx| { if can_flash { this.confirm(false, cx); } })))
                .child(button("restore", "Restore saved backup", can_restore)
                    .on_click(cx.listener(move |this, _, _, cx| { if can_restore { this.confirm(true, cx); } }))))
            .child(div().text_sm().text_color(rgb(if self.error { 0xffbec2 } else { MUTED }))
                .when(self.error || self.is_busy(), |el| el.bg(rgb(0x1d3341)).rounded_md().p_3())
                .child(self.status.clone()))
            .child(div().flex().justify_between().items_center().gap_3()
                .child(button("files", "Open backups folder", enabled)
                    .on_click(cx.listener(move |this, _, _, cx| { if enabled { cx.open_with_system(&this.root.join("backups")); } })))
                .child(button("tools", if self.show_tools { "Hide tools & logs" } else { "Tools & logs" }, enabled)
                    .on_click(cx.listener(move |this, _, _, cx| { if enabled { this.show_tools = !this.show_tools; cx.notify(); } }))));

        // Recovery stays visible after a failed install; technical tools are otherwise collapsed.
        if self.show_tools || self.error || self.recovery_required {
            let recent: Vec<_> = self
                .output
                .lines()
                .filter(|line| !line.trim().is_empty())
                .rev()
                .take(8)
                .map(String::from)
                .collect();
            content = content.child(div().p_4().rounded_md().bg(rgb(0x1d3341)).flex().flex_col().gap_3()
                .when(self.recovery_required, |el| el.child(div().text_sm().text_color(rgb(0xffbec2))
                    .child("Installation failed. Restore a saved backup before continuing.")))
                .child(div().flex().gap_3()
                    .child(button("custom", "Choose HEX / BIN", normal_operations)
                        .on_click(cx.listener(move |this, _, _, cx| { if normal_operations { this.choose_file(false, cx); } })))
                    .child(button("exit", "Leave DFU", normal_operations)
                        .on_click(cx.listener(move |this, _, _, cx| { if normal_operations { this.start(Operation::Leave, cx); } })))
                    .child(button("logs", "Open last session", enabled && self.session.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| { if enabled && let Some(path) = &this.session { cx.open_with_system(path); } }))))
                .child(div().text_xs().text_color(rgb(MUTED)).child(format!("Selected image SHA-256: {}", self.firmware.hash)))
                .child(div().text_xs().text_color(rgb(MUTED)).child("Verification: device block CRCs, without full byte readback. Backups and logs are stored locally."))
                .children(recent.into_iter().rev().map(|line| div().text_xs().text_color(rgb(MUTED)).child(line))));
        }
        // Keep the confirmation outside the scrolling page so it covers the whole window.
        let mut page = div()
            .relative()
            .size_full()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(0xffffff))
            .font_family("Lato")
            .child(
                div()
                    .id("page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .p_8()
                    .child(content),
            );
        if std::env::args().any(|arg| arg == "--preview-details") {
            // Render the actual tooltip component in view without a mouse or USB device.
            page = page.child(
                div()
                    .absolute()
                    .top(px(210.))
                    .right(px(30.))
                    .child(cx.new(|_| FirmwareDetails(Choice::Fast))),
            );
        }
        if let Some(Operation::Install {
            firmware, recovery, ..
        }) = &self.confirmation
        {
            let acknowledged = self.risk_acknowledged;
            let description = format!(
                "{} {}?",
                if *recovery { "Restore" } else { "Flash" },
                firmware.name
            );
            page = page.child(div().absolute().inset_0().p_4().bg(gpui::rgba(0x10202dcc)).flex().items_center().justify_center()
                .child(div().id("confirmation-dialog").w(px(620.)).max_h_full().overflow_y_scroll().rounded_lg().border_2().border_color(rgb(0xe78d95)).bg(rgb(BACKGROUND)).p_6().flex().flex_col().gap_4()
                    .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child(description))
                    .child(div().p_4().rounded_md().border_2().border_color(rgb(0xe56b78)).bg(rgb(0x4a2633)).flex().flex_col().gap_3()
                        .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).text_color(rgb(0xffbdc4)).child("DANGER: RISK OF BRICKING"))
                        .child(div().text_base().font_weight(gpui::FontWeight::BOLD).child("An interrupted or failed firmware installation can permanently brick the tablet and leave it unusable."))
                        .child(div().text_sm().text_color(rgb(0xffdce0)).child("Recovery may require opening the tablet or using an external programmer. A backup does not guarantee recovery."))
                        .child(div().text_sm().font_weight(gpui::FontWeight::BOLD).child("Unofficial firmware may void the manufacturer warranty. Damage caused by modifications may not be covered.")))
                    .child(div().text_sm().text_color(rgb(MUTED)).child("Installation automatically enters DFU, validates the backup, writes firmware and verifies flash block CRCs before returning to normal mode."))
                    .child(div().text_sm().child("Keep the USB cable connected throughout the operation."))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(if *recovery { "Recovery checks the resident boot code before restoring the saved application." } else { "A backup mismatch cancels installation before erasure." }))
                    .child(div().id("risk-acknowledgement").flex().items_center().gap_3().p_3().rounded_md().border_1().border_color(rgb(if acknowledged { ACCENT } else { 0x7896a4 })).cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| { if this.confirmation.is_some() && !this.is_busy() { this.risk_acknowledged = !this.risk_acknowledged; cx.notify(); } }))
                        .child(div().w(px(21.)).h(px(21.)).flex_none().rounded_sm().border_2().border_color(rgb(if acknowledged { ACCENT } else { 0x91abb7 })).bg(rgb(if acknowledged { ACCENT } else { BACKGROUND })).flex().items_center().justify_center().text_sm().when(acknowledged, |el| el.child("✓")))
                        .child(div().flex_1().text_sm().child("I accept the risk of permanent device damage and warranty loss.")))
                    .child(div().flex().gap_3()
                        .child(button("cancel", "Cancel", true).on_click(cx.listener(|this, _, _, cx| { this.confirmation = None; this.risk_acknowledged = false; cx.notify(); })))
                        .child(button("confirm", "Accept risk and flash", acknowledged)
                            .when(acknowledged, |el| el.bg(rgb(0xb63e53)).border_color(rgb(0xe56b78)).hover(|el| el.bg(rgb(0x9c3045))))
                            .on_click(cx.listener(|this, _, _, cx| this.flash_confirmed(cx)))))));
        }
        page
    }
}

struct BackupDetails(String);
impl Render for BackupDetails {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(500.))
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(ACCENT))
            .bg(rgb(0x1b303d))
            .text_color(rgb(0xffffff))
            .font_family("Lato")
            .text_xs()
            .child(self.0.clone())
    }
}
