use super::Operation;
use xppen_firmware_core::progress::{Progress, Stage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    Active,
    Complete,
    Failed,
}

pub struct Step {
    pub stage: Stage,
    pub state: State,
    pub work: Option<(usize, usize)>,
}

impl Step {
    pub fn fraction(&self) -> f32 {
        if self.state == State::Complete {
            return 1.;
        }
        self.work
            .map_or(0., |(done, total)| done as f32 / total as f32)
    }

    pub fn detail(&self, stopped: bool) -> String {
        match self.state {
            State::Pending => if stopped { "Not run" } else { "Waiting" }.into(),
            State::Complete => "100%".into(),
            State::Failed => "Failed".into(),
            State::Active => match self.work {
                Some((done, total)) => format!(
                    "{:.0}% · {done}/{total} {}",
                    self.fraction() * 100.,
                    match self.stage {
                        Stage::ReadBackup => "bytes",
                        Stage::WriteFirmware => "bytes",
                        _ => "checks",
                    }
                ),
                None => "Working…".into(),
            },
        }
    }
}

/// Tracks step outcomes independently of transfer counters so successful
/// cleanup cannot hide an earlier operation failure.
pub struct OperationProgress {
    pub title: &'static str,
    pub steps: Vec<Step>,
    pub stopped: bool,
    pub succeeded: bool,
}

impl OperationProgress {
    pub fn new(operation: &Operation) -> Self {
        let (title, stages): (&str, &[Stage]) = match operation {
            Operation::Identify => (
                "Check connection",
                &[Stage::Prepare, Stage::Identify, Stage::SaveLog],
            ),
            Operation::Backup => (
                "Create backup",
                &[
                    Stage::Prepare,
                    Stage::EnterDfu,
                    Stage::ReadBackup,
                    Stage::CheckBackup,
                    Stage::SaveBackup,
                    Stage::ReturnRuntime,
                    Stage::SaveLog,
                ],
            ),
            Operation::Install { recovery, .. } => (
                if *recovery {
                    "Restore backup"
                } else {
                    "Flash firmware"
                },
                &[
                    Stage::Prepare,
                    Stage::SaveRollback,
                    Stage::EnterDfu,
                    if *recovery {
                        Stage::CheckResident
                    } else {
                        Stage::CheckCurrent
                    },
                    Stage::WriteFirmware,
                    Stage::ReconnectDfu,
                    Stage::CheckWritten,
                    Stage::SaveVerification,
                    Stage::ReturnRuntime,
                    Stage::SaveLog,
                ],
            ),
            Operation::Leave => (
                "Leave DFU",
                &[Stage::Prepare, Stage::ReturnRuntime, Stage::SaveLog],
            ),
        };
        Self {
            title,
            steps: stages
                .iter()
                .map(|&stage| Step {
                    stage,
                    state: State::Pending,
                    work: None,
                })
                .collect(),
            stopped: false,
            succeeded: false,
        }
    }

    pub fn apply(&mut self, event: Progress) {
        if self.stopped {
            return;
        }
        let stage = match event {
            Progress::Started(stage)
            | Progress::Finished(stage)
            | Progress::Failed(stage)
            | Progress::Advanced { stage, .. } => stage,
        };
        let Some(step) = self.steps.iter_mut().find(|step| step.stage == stage) else {
            return;
        };
        match event {
            Progress::Started(_) if step.state == State::Pending => step.state = State::Active,
            Progress::Advanced {
                completed, total, ..
            } if step.state == State::Active && total > 0 && completed <= total => {
                // Ignore stale, contradictory counters instead of letting the bar jump backwards.
                if step.work.is_none_or(|(previous, previous_total)| {
                    total == previous_total && completed >= previous
                }) {
                    step.work = Some((completed, total));
                }
            }
            Progress::Finished(_) if step.state == State::Active => step.state = State::Complete,
            Progress::Failed(_) if step.state == State::Active => step.state = State::Failed,
            _ => {}
        }
    }

    pub fn finish(&mut self, success: bool) {
        self.stopped = true;
        self.succeeded = success && self.steps.iter().all(|step| step.state == State::Complete);
        for step in &mut self.steps {
            if step.state == State::Active {
                step.state = State::Failed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_counter_is_not_success_and_cleanup_does_not_hide_failure() {
        let mut view = OperationProgress::new(&Operation::Backup);
        view.apply(Progress::Started(Stage::ReadBackup));
        view.apply(Progress::Advanced {
            stage: Stage::ReadBackup,
            completed: 65536,
            total: 65536,
        });
        assert_eq!(view.steps[2].state, State::Active);
        view.apply(Progress::Failed(Stage::ReadBackup));
        view.apply(Progress::Started(Stage::ReturnRuntime));
        view.apply(Progress::Finished(Stage::ReturnRuntime));
        view.finish(false);
        assert_eq!(view.steps[2].state, State::Failed);
        assert_eq!(view.steps[5].state, State::Complete);
        assert_eq!(view.steps[4].state, State::Pending);
        assert!(!view.succeeded);
    }

    #[test]
    fn counters_do_not_regress_or_complete_a_failed_step() {
        let mut view = OperationProgress::new(&Operation::Backup);
        view.apply(Progress::Started(Stage::CheckBackup));
        view.apply(Progress::Advanced {
            stage: Stage::CheckBackup,
            completed: 8,
            total: 16,
        });
        for (completed, total) in [(7, 16), (9, 0), (17, 16), (9, 32)] {
            view.apply(Progress::Advanced {
                stage: Stage::CheckBackup,
                completed,
                total,
            });
        }
        assert_eq!(view.steps[3].fraction(), 0.5);
        view.apply(Progress::Failed(Stage::CheckBackup));
        view.apply(Progress::Finished(Stage::CheckBackup));
        assert_eq!(view.steps[3].state, State::Failed);
    }
}
