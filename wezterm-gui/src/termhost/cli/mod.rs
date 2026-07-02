//! Windows default terminal host management.

use wezterm_gui_subcommands::{TerminalHostCommand, TerminalHostSub};

mod disable;
mod enable;

pub(crate) struct KnownHost {
    pub(crate) id: &'static str,
    pub(crate) console_clsid: &'static str,
}

pub(crate) const KNOWN_HOSTS: &[KnownHost] = &[
    KnownHost {
        id: "wt-release",
        console_clsid: "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}",
    },
    KnownHost {
        id: "wt-preview",
        console_clsid: "{06EC847C-C0A5-46B8-92CB-7C92F6E35CD5}",
    },
    KnownHost {
        id: "wt-canary",
        console_clsid: "{A854D02A-F2FE-44A5-BB24-D03F4CF830D4}",
    },
    KnownHost {
        id: "wt-dev",
        console_clsid: "{1F9F2BF5-5BC3-4F17-B0E6-912413F1F451}",
    },
];

pub fn run(cmd: TerminalHostCommand) -> anyhow::Result<()> {
    match cmd.sub {
        TerminalHostSub::Enable => enable::EnableCommand::run(),
        TerminalHostSub::Disable => disable::DisableCommand::run(),
    }
}
