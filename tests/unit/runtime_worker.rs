use std::sync::mpsc::Receiver;

type TestGate = (Sender<()>, Receiver<()>);

pub(crate) enum TestWork {
    Hold(Receiver<()>),
    Staged(Vec<u8>, u32, u64, u64, Option<TestGate>, bool),
    Phased(Vec<u8>, u32, u64, u64, u8, Sender<()>, Receiver<()>, bool),
    AppendPhased(Vec<u8>, u64, u64, u8, Sender<()>, Receiver<()>),
    ClearPhased(u64, u64, u8, Sender<()>, Receiver<()>),
    Recover(Vec<u8>, u32, u64, u64, Sender<()>, Receiver<()>),
}

fn phased_io(
    state: &State,
    deadline: Instant,
    stage: u8,
    step: StoreStep,
    entered: &Sender<()>,
    gate: &Receiver<()>,
    announced: &mut bool,
) -> Result<(), StoreError> {
    if matches!(
        (stage, step),
        (1, StoreStep::Commit) | (3, StoreStep::Flush)
    ) && !*announced
    {
        *announced = true;
        let _ = entered.send(());
        let _ = gate.recv();
    }
    state.io(deadline, step)?;
    if stage == 2 && step == StoreStep::Flush && !*announced {
        *announced = true;
        let _ = entered.send(());
        let _ = gate.recv();
    }
    Ok(())
}

impl TestWork {
    fn size(&self) -> usize {
        match self {
            Self::Recover(bytes, ..) | Self::AppendPhased(bytes, ..) => bytes.len(),
            _ => 0,
        }
    }

    fn run(self, store: &mut Store, state: &State, deadline: Instant) -> Outcome {
        match self {
            Self::Staged(bytes, epoch, start, end, _, fail) => {
                let commit = *store
                    .replace_with(&bytes, epoch, start, end, |step| state.io(deadline, step))?;
                if fail {
                    Err(StoreError::Corrupt)
                } else {
                    Ok((commit, false))
                }
            }
            Self::Phased(bytes, epoch, start, end, stage, entered, gate, fail) => {
                let mut announced = false;
                let commit = *store.replace_with(&bytes, epoch, start, end, |step| {
                    phased_io(
                        state,
                        deadline,
                        stage,
                        step,
                        &entered,
                        &gate,
                        &mut announced,
                    )
                })?;
                if fail {
                    Err(StoreError::Corrupt)
                } else {
                    Ok((commit, false))
                }
            }
            Self::AppendPhased(bytes, cap, end, stage, entered, gate) => {
                let mut announced = false;
                store
                    .append_capped_with(&bytes, cap, end, |step| {
                        phased_io(
                            state,
                            deadline,
                            stage,
                            step,
                            &entered,
                            &gate,
                            &mut announced,
                        )
                    })
                    .map(|commit| (*commit, false))
            }
            Self::ClearPhased(observed, end, stage, entered, gate) => {
                let selected = *store.selected();
                if selected.index != observed || selected.length == 0 {
                    return Ok((selected, selected.index != observed));
                }
                let epoch = selected.epoch.checked_add(1).ok_or(StoreError::Exhausted)?;
                let mut announced = false;
                store
                    .replace_with(&[], epoch, end, end, |step| {
                        phased_io(
                            state,
                            deadline,
                            stage,
                            step,
                            &entered,
                            &gate,
                            &mut announced,
                        )
                    })
                    .map(|commit| (*commit, false))
            }
            Self::Recover(bytes, epoch, start, end, entered, gate) => {
                store.replace_with(&bytes, epoch, start, end, |step| state.io(deadline, step))?;
                let _ = entered.send(());
                let _ = gate.recv();
                Err(StoreError::Corrupt)
            }
            Self::Hold(wait) => {
                let _ = wait.recv();
                Ok((*store.selected(), false))
            }
        }
    }
}

#[allow(non_snake_case, clippy::too_many_arguments)]
impl Work {
    pub(crate) fn Hold(wait: Receiver<()>) -> Self {
        Self::Test(TestWork::Hold(wait))
    }

    pub(crate) fn Staged(
        bytes: Vec<u8>,
        epoch: u32,
        start: u64,
        end: u64,
        gate: Option<TestGate>,
        fail: bool,
    ) -> Self {
        Self::Test(TestWork::Staged(bytes, epoch, start, end, gate, fail))
    }

    pub(crate) fn Phased(
        bytes: Vec<u8>,
        epoch: u32,
        start: u64,
        end: u64,
        stage: u8,
        entered: Sender<()>,
        gate: Receiver<()>,
        fail: bool,
    ) -> Self {
        Self::Test(TestWork::Phased(
            bytes, epoch, start, end, stage, entered, gate, fail,
        ))
    }

    pub(crate) fn AppendPhased(
        bytes: Vec<u8>,
        cap: u64,
        end: u64,
        stage: u8,
        entered: Sender<()>,
        gate: Receiver<()>,
    ) -> Self {
        Self::Test(TestWork::AppendPhased(
            bytes, cap, end, stage, entered, gate,
        ))
    }

    pub(crate) fn ClearPhased(
        observed: u64,
        end: u64,
        stage: u8,
        entered: Sender<()>,
        gate: Receiver<()>,
    ) -> Self {
        Self::Test(TestWork::ClearPhased(observed, end, stage, entered, gate))
    }

    pub(crate) fn Recover(
        bytes: Vec<u8>,
        epoch: u32,
        start: u64,
        end: u64,
        entered: Sender<()>,
        gate: Receiver<()>,
    ) -> Self {
        Self::Test(TestWork::Recover(bytes, epoch, start, end, entered, gate))
    }

    fn prepare_test(self) -> Self {
        match self {
            Self::Test(TestWork::Staged(bytes, epoch, start, end, Some((entered, gate)), fail)) => {
                let _ = entered.send(());
                let _ = gate.recv();
                Self::Staged(bytes, epoch, start, end, None, fail)
            }
            operation => operation,
        }
    }
}

impl State {
    fn pause_publication(&self) {
        let gate = self
            .publication_gate
            .lock()
            .expect("publication gate")
            .take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.recv();
        }
    }
}

impl Lane {
    #[allow(dead_code)]
    pub(crate) fn block_publication(&self, entered: Sender<()>, release: Receiver<()>) {
        let published = Arc::clone(&self.published);
        std::thread::spawn(move || {
            let _guard = published.lock().expect("published lock");
            let _ = entered.send(());
            let _ = release.recv();
        });
    }

    #[allow(dead_code)]
    pub(crate) fn delay_publication(&self, entered: Sender<()>, release: Receiver<()>) {
        *self
            .state
            .publication_gate
            .lock()
            .expect("publication gate") = Some((entered, release));
    }
}
