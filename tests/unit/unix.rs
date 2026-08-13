// Included by src/unix.rs under #[cfg(test)]; not compiled into production.

#[cfg(test)]
mod interrupted_connect_tests {
    use super::*;
    use std::cell::Cell;

    // EINTR reports that a signal arrived mid-call; it says nothing about the
    // peer. These cases drive the retry through an injected clock and an
    // injected outcome sequence, so nothing here depends on the scheduler.
    #[test]
    fn an_interruption_storm_stops_at_the_call_budget() {
        // The budget counts CALLS, not retries: exactly MAX_CONNECT_ATTEMPTS
        // invocations, then the last interruption is returned. A range would
        // witness nothing — the number could drift underneath it.
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3600);
        let calls = Cell::new(0);
        let outcome = retry_interrupted(deadline, || start, || {
            calls.set(calls.get() + 1);
            Err::<(), io::Error>(io::Error::from(io::ErrorKind::Interrupted))
        });
        // The literal, not the symbol: comparing against MAX_CONNECT_ATTEMPTS
        // would be self-referential — change the constant and such a test
        // changes with it, witnessing nothing. Pinning 16 makes moving the
        // budget a deliberate edit here, which is what a contract number
        // deserves.
        assert_eq!(MAX_CONNECT_ATTEMPTS, 16, "the connect budget is 16 calls");
        assert_eq!(calls.get(), 16, "exactly 16 total connect calls");
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn a_reached_deadline_stops_before_the_next_call() {
        // Injected time: the clock is already at the deadline when admission is
        // consulted, so attempt two never starts and the interruption stands —
        // indeterminate, never a liveness claim.
        let start = Instant::now();
        let deadline = start + Duration::from_millis(500);
        let calls = Cell::new(0);
        let outcome = retry_interrupted(deadline, || deadline, || {
            calls.set(calls.get() + 1);
            Err::<(), io::Error>(io::Error::from(io::ErrorKind::Interrupted))
        });
        assert_eq!(calls.get(), 1, "admission must refuse a retry at the deadline");
        assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(!connect_refused(&io::Error::from(io::ErrorKind::Interrupted)));
    }

    #[test]
    fn time_remaining_admits_the_retry_that_answers() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(2);
        let calls = Cell::new(0);
        let outcome = retry_interrupted(deadline, || start, || {
            let index = calls.get();
            calls.set(index + 1);
            match index {
                0 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                _ => Ok(7u32),
            }
        });
        assert_eq!(outcome.expect("the retry must reach its answer"), 7);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn a_non_interrupted_error_is_answered_on_the_first_call() {
        // Permission, timeout and the rest stay exactly one call: the retry
        // must not turn an indeterminate answer into a liveness claim, and must
        // not spin on an error that will not change.
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3600);
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::NotFound,
            io::ErrorKind::ConnectionRefused,
        ] {
            let calls = Cell::new(0);
            let outcome = retry_interrupted(deadline, || start, || {
                calls.set(calls.get() + 1);
                Err::<(), io::Error>(io::Error::from(kind))
            });
            assert_eq!(calls.get(), 1, "{kind:?} must not be retried");
            assert_eq!(outcome.unwrap_err().kind(), kind);
        }
    }

    #[test]
    fn the_protocol_phase_receives_the_connect_phase_deadline() {
        // The two phases must share ONE deadline. Structure alone does not
        // prove it: restoring a freshly computed window in the protocol phase
        // would leave every other test here green while silently handing the
        // identity exchange a budget the caller never granted. This captures
        // both arguments and compares them — no wall clock, and no claim about
        // a syscall already in flight.
        let deadline = Instant::now() + Duration::from_secs(2);
        let dialled = Cell::new(None);
        let spoken = Cell::new(None);
        let outcome: std::result::Result<&str, ()> = within_deadline(
            deadline,
            |given| {
                dialled.set(Some(given));
                Ok("stream")
            },
            |given, carried| {
                spoken.set(Some(given));
                assert_eq!(carried, "stream", "the connect phase result must carry over");
                Ok("client")
            },
        );
        assert_eq!(outcome, Ok("client"));
        assert_eq!(dialled.get(), Some(deadline), "connect phase got the deadline");
        assert_eq!(
            spoken.get(),
            Some(deadline),
            "the protocol phase must receive the ORIGINAL deadline, not a fresh one"
        );
    }

    #[test]
    fn a_failed_connect_phase_never_reaches_the_protocol_phase() {
        // A refusal ends the probe; running the identity exchange against a
        // stream that was never established would be a second failure mode
        // reported as the first.
        let deadline = Instant::now() + Duration::from_secs(2);
        let spoke = Cell::new(false);
        let outcome: std::result::Result<&str, &str> = within_deadline(
            deadline,
            |_| Err("refused"),
            |_, _: &str| {
                spoke.set(true);
                Ok("client")
            },
        );
        assert_eq!(outcome, Err("refused"));
        assert!(!spoke.get(), "the protocol phase must not run after a failed connect");
    }

    #[test]
    fn only_a_refusal_proves_nothing_is_listening() {
        // The bool this feeds is the whole classification channel, so widening
        // it would turn an open question into a liveness claim.
        assert!(connect_refused(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::NotFound,
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::AddrInUse,
            io::ErrorKind::AddrNotAvailable,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::Other,
        ] {
            assert!(
                !connect_refused(&io::Error::from(kind)),
                "{kind:?} must stay indeterminate, never stale"
            );
        }
    }
}

#[cfg(test)]
mod headless_terminal_tests {
    use super::*;

    #[test]
    fn headless_termios_is_exactly_the_frozen_closure_6_3_set() {
        let terminal = headless_termios();
        assert_eq!(terminal.c_iflag, libc::ICRNL | libc::IXON, "c_iflag");
        assert_eq!(terminal.c_oflag, libc::OPOST | libc::ONLCR, "c_oflag");
        // Darwin keeps speeds outside c_cflag, so the word compares exactly;
        // Linux encodes them as CBAUD/CBAUDEX bits which must be masked out.
        #[cfg(target_os = "macos")]
        assert_eq!(
            terminal.c_cflag,
            libc::CS8 | libc::CREAD,
            "c_cflag holds exactly CS8|CREAD"
        );
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            terminal.c_cflag & !(libc::CBAUD | libc::CBAUDEX),
            libc::CS8 | libc::CREAD,
            "c_cflag holds exactly CS8|CREAD beside the speed bits"
        );
        assert_eq!(
            terminal.c_lflag,
            libc::ISIG
                | libc::ICANON
                | libc::IEXTEN
                | libc::ECHO
                | libc::ECHOE
                | libc::ECHOK
                | libc::ECHOCTL
                | libc::ECHOKE,
            "c_lflag"
        );
        assert_eq!(unsafe { libc::cfgetispeed(&terminal) }, libc::B38400);
        assert_eq!(unsafe { libc::cfgetospeed(&terminal) }, libc::B38400);

        let listed = [
            (libc::VINTR, 0x03),
            (libc::VQUIT, 0x1C),
            (libc::VERASE, 0x7F),
            (libc::VKILL, 0x15),
            (libc::VEOF, 0x04),
            (libc::VSTART, 0x11),
            (libc::VSTOP, 0x13),
            (libc::VSUSP, 0x1A),
            (libc::VREPRINT, 0x12),
            (libc::VDISCARD, 0x0F),
            (libc::VWERASE, 0x17),
            (libc::VLNEXT, 0x16),
            (libc::VMIN, 0x01),
            (libc::VTIME, 0x00),
        ];
        for (slot, byte) in listed {
            assert_eq!(terminal.c_cc[slot], byte, "control slot {slot}");
        }
        // Every slot outside the listed set stays disabled — VEOL and VEOL2 by
        // name, and on Linux VSWTC — so the kernel default cannot leak in.
        #[cfg(target_os = "macos")]
        let disabled: libc::cc_t = 0xff;
        #[cfg(not(target_os = "macos"))]
        let disabled: libc::cc_t = 0;
        #[allow(unused_mut)]
        let mut exempt: Vec<usize> = listed.iter().map(|(slot, _)| *slot).collect();
        #[cfg(target_os = "macos")]
        exempt.extend([libc::VDSUSP, libc::VSTATUS]);
        for slot in 0..libc::NCCS {
            if !exempt.contains(&slot) {
                assert_eq!(terminal.c_cc[slot], disabled, "unlisted slot {slot}");
            }
        }
        assert_eq!(terminal.c_cc[libc::VEOL], disabled, "VEOL stays disabled");
        assert_eq!(terminal.c_cc[libc::VEOL2], disabled, "VEOL2 stays disabled");
        // The macOS-specific controls carry their frozen values, not merely an
        // exemption from the disabled loop.
        #[cfg(target_os = "macos")]
        {
            assert_eq!(terminal.c_cc[libc::VDSUSP], 0x19, "VDSUSP");
            assert_eq!(terminal.c_cc[libc::VSTATUS], 0x14, "VSTATUS");
        }
    }

    #[test]
    fn headless_creation_applies_the_frozen_set_not_the_kernel_default() {
        // terminal_config(false) must hand openpty the frozen termios; None
        // here would silently adopt whatever the platform's kernel defaults
        // to, which closure §6.3 explicitly forbids.
        let (modes, size) = terminal_config(false).unwrap();
        let modes = modes.expect("headless creation must carry the frozen termios");
        assert_eq!(modes.c_iflag, headless_termios().c_iflag);
        assert_eq!((size.ws_row, size.ws_col), (24, 80));
    }
}
