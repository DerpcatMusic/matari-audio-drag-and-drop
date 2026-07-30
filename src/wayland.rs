//! Event-driven lifecycle state for native Wayland data sources.

use crate::{LinuxOutcome, Outcome, WaylandReporter};

/// Reports one native Wayland source lifecycle without guessing completion.
///
/// A source becomes replaceable only after the compositor reports a performed
/// drop and the destination has received at least one requested payload. This
/// is not terminal: runtimes must retain the native source until Wayland sends
/// `dnd_finished` or `cancelled` so late MIME requests can still be served.
pub struct WaylandSourceReporter {
    reporter: WaylandReporter,
    data_requested: bool,
    drop_performed: bool,
    transfer_ready: bool,
}

impl WaylandSourceReporter {
    /// Wrap the reporter supplied to [`crate::WaylandRuntimePort::start_drag`].
    #[must_use]
    pub const fn new(reporter: WaylandReporter) -> Self {
        Self {
            reporter,
            data_requested: false,
            drop_performed: false,
            transfer_ready: false,
        }
    }

    /// Report that the destination received one requested MIME payload.
    pub fn data_requested(&mut self) {
        self.reporter.data_requested();
        self.data_requested = true;
        self.report_transfer_ready();
    }

    /// Report the compositor's `dnd_drop_performed` event.
    pub fn drop_performed(&mut self) {
        self.reporter.drop_performed();
        self.drop_performed = true;
        self.report_transfer_ready();
    }

    /// Whether protocol evidence permits a newer gesture to replace this one.
    ///
    /// The native source must remain alive after this becomes true.
    #[must_use]
    pub const fn is_transfer_ready(&self) -> bool {
        self.transfer_ready
    }

    /// Finish the source from an authoritative Linux terminal event.
    pub fn finish_linux(&self, outcome: LinuxOutcome) {
        self.reporter.finish_linux(outcome);
    }

    /// Finish the source from an authoritative native terminal event.
    pub fn finish(&self, outcome: Outcome) {
        self.reporter.finish(outcome);
    }

    fn report_transfer_ready(&mut self) {
        if !self.transfer_ready && self.data_requested && self.drop_performed {
            self.transfer_ready = true;
            self.reporter.transfer_ready();
        }
    }
}
