//! Observability metrics endpoint (ops02).
//!
//! Per the implementation DAG, this module provides a minimal
//! Prometheus-compatible metrics exposition endpoint.  The endpoint
//! reads from the `Metrics` registry (P0 f08) and emits in the
//! Prometheus text-based exposition format for scraping by `node_exporter`
//! or equivalent.
//!
//! Format reference: <https://prometheus.io/docs/instrumenting/exposition_formats/>
//!
//! V1 scope:
//!
//! - Counter metrics only.  Histograms / gauges deferred.
//! - Plain HTTP, no TLS.  Exposed only on local-only socket per the
//!   same gating as the snapshot RPC (P4 sn13).
//!
//! Spec context: full dashboards land with the ops deployment
//! workflow (ops01).  This module is the foundation.

use crate::telemetry::Metrics;

/// Render the `Metrics` registry as Prometheus exposition format.
pub fn render_prometheus(metrics: &Metrics) -> String {
    let snap = metrics.snapshot();
    let mut out = String::with_capacity(snap.len() * 64);
    for (name, value) in snap {
        let metric_name = sanitize_metric_name(&name);
        // # HELP and # TYPE lines per Prometheus convention.
        out.push_str(&format!("# HELP {metric_name} caduceus daemon metric\n"));
        out.push_str(&format!("# TYPE {metric_name} counter\n"));
        out.push_str(&format!("{metric_name} {value}\n"));
    }
    out
}

/// Convert a counter name into a Prometheus-compatible metric name.
/// Replaces `.` and any other non-alnum/`_` char with `_`.
fn sanitize_metric_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    out.push_str("caduceusd_");
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_metric_name_replaces_dots() {
        assert_eq!(
            sanitize_metric_name("dispatch.attempts"),
            "caduceusd_dispatch_attempts"
        );
    }

    #[test]
    fn sanitize_metric_name_preserves_alnum_and_underscore() {
        assert_eq!(sanitize_metric_name("foo_bar123"), "caduceusd_foo_bar123");
    }

    #[test]
    fn render_prometheus_emits_counter_with_help_and_type() {
        let m = Metrics::new();
        m.counter("dispatch.attempts").add(5);
        let out = render_prometheus(&m);
        assert!(out.contains("# HELP caduceusd_dispatch_attempts"));
        assert!(out.contains("# TYPE caduceusd_dispatch_attempts counter"));
        assert!(out.contains("caduceusd_dispatch_attempts 5"));
    }

    #[test]
    fn render_prometheus_emits_zero_for_unincremented_counters() {
        let m = Metrics::new();
        m.counter("startup.success");
        let out = render_prometheus(&m);
        assert!(out.contains("caduceusd_startup_success 0"));
    }

    #[test]
    fn render_prometheus_sorted_by_name() {
        let m = Metrics::new();
        m.counter("z.last").incr();
        m.counter("a.first").incr();
        let out = render_prometheus(&m);
        let a_pos = out.find("caduceusd_a_first").unwrap();
        let z_pos = out.find("caduceusd_z_last").unwrap();
        assert!(a_pos < z_pos);
    }

    #[test]
    fn render_prometheus_handles_empty_registry() {
        let m = Metrics::new();
        let out = render_prometheus(&m);
        assert!(out.is_empty());
    }
}
