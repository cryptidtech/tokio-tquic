use tquic::CongestionControlAlgorithm;
use tquic::MultipathAlgorithm;

use crate::TransportConfig;

/// Extension trait for TQUIC-specific transport configuration.
///
/// These settings expose TQUIC-specific capabilities not available in Quinn,
/// including congestion control algorithm selection, multipath QUIC, and
/// fine-tuning of BBR/COPA parameters.
pub trait TransportConfigExt {
    /// Set the congestion control algorithm (Cubic, BBR, BBRv3, or COPA).
    fn set_congestion_control_algorithm(&mut self, algo: CongestionControlAlgorithm) -> &mut Self;

    /// Set the initial congestion window in packets.
    fn set_initial_congestion_window(&mut self, packets: u64) -> &mut Self;

    /// Set the minimum congestion window in packets.
    fn set_min_congestion_window(&mut self, packets: u64) -> &mut Self;

    /// Enable or disable packet pacing.
    fn enable_pacing(&mut self, enabled: bool) -> &mut Self;

    /// Enable multipath QUIC.
    fn enable_multipath(&mut self, enabled: bool) -> &mut Self;

    /// Set the multipath scheduling algorithm (MinRtt, Redundant, RoundRobin).
    fn set_multipath_algorithm(&mut self, algo: MultipathAlgorithm) -> &mut Self;

    /// Set BBR ProbeRTT duration in milliseconds.
    fn set_bbr_probe_rtt_duration(&mut self, millis: u64) -> &mut Self;

    /// Enable BBR ProbeRTT based on BDP.
    fn enable_bbr_probe_rtt_based_on_bdp(&mut self, enabled: bool) -> &mut Self;

    /// Set BBR ProbeRTT congestion window gain.
    fn set_bbr_probe_rtt_cwnd_gain(&mut self, gain: f64) -> &mut Self;

    /// Set BBR RTprop filter length in milliseconds.
    fn set_bbr_rtprop_filter_len(&mut self, millis: u64) -> &mut Self;

    /// Set BBR ProbeBW congestion window gain.
    fn set_bbr_probe_bw_cwnd_gain(&mut self, gain: f64) -> &mut Self;

    /// Set COPA slow-start delta parameter.
    fn set_copa_slow_start_delta(&mut self, delta: f64) -> &mut Self;

    /// Set COPA steady-state delta parameter.
    fn set_copa_steady_delta(&mut self, delta: f64) -> &mut Self;

    /// Enable COPA standing RTT usage.
    fn enable_copa_use_standing_rtt(&mut self, enabled: bool) -> &mut Self;
}

impl TransportConfigExt for TransportConfig {
    fn set_congestion_control_algorithm(&mut self, algo: CongestionControlAlgorithm) -> &mut Self {
        self.congestion_control_algorithm = Some(algo);
        self
    }

    fn set_initial_congestion_window(&mut self, packets: u64) -> &mut Self {
        self.initial_congestion_window = Some(packets);
        self
    }

    fn set_min_congestion_window(&mut self, packets: u64) -> &mut Self {
        self.min_congestion_window = Some(packets);
        self
    }

    fn enable_pacing(&mut self, enabled: bool) -> &mut Self {
        self.enable_pacing = Some(enabled);
        self
    }

    fn enable_multipath(&mut self, enabled: bool) -> &mut Self {
        self.enable_multipath = Some(enabled);
        self
    }

    fn set_multipath_algorithm(&mut self, algo: MultipathAlgorithm) -> &mut Self {
        self.multipath_algorithm = Some(algo);
        self
    }

    fn set_bbr_probe_rtt_duration(&mut self, millis: u64) -> &mut Self {
        self.bbr_probe_rtt_duration = Some(millis);
        self
    }

    fn enable_bbr_probe_rtt_based_on_bdp(&mut self, enabled: bool) -> &mut Self {
        self.bbr_probe_rtt_based_on_bdp = Some(enabled);
        self
    }

    fn set_bbr_probe_rtt_cwnd_gain(&mut self, gain: f64) -> &mut Self {
        self.bbr_probe_rtt_cwnd_gain = Some(gain);
        self
    }

    fn set_bbr_rtprop_filter_len(&mut self, millis: u64) -> &mut Self {
        self.bbr_rtprop_filter_len = Some(millis);
        self
    }

    fn set_bbr_probe_bw_cwnd_gain(&mut self, gain: f64) -> &mut Self {
        self.bbr_probe_bw_cwnd_gain = Some(gain);
        self
    }

    fn set_copa_slow_start_delta(&mut self, delta: f64) -> &mut Self {
        self.copa_slow_start_delta = Some(delta);
        self
    }

    fn set_copa_steady_delta(&mut self, delta: f64) -> &mut Self {
        self.copa_steady_delta = Some(delta);
        self
    }

    fn enable_copa_use_standing_rtt(&mut self, enabled: bool) -> &mut Self {
        self.copa_use_standing_rtt = Some(enabled);
        self
    }
}
