// Included by src/unix.rs under #[cfg(test)]; not compiled into production.

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
