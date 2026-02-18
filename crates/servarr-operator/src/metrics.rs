use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts};

lazy_static::lazy_static! {
    pub static ref RECONCILE_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_reconcile_total",
            "Total number of reconciliations"
        ),
        &["app_type", "result"]
    )
    .unwrap();

    pub static ref RECONCILE_DURATION: HistogramVec = prometheus::register_histogram_vec!(
        HistogramOpts::new(
            "servarr_operator_reconcile_duration_seconds",
            "Duration of reconciliations in seconds"
        ),
        &["app_type"]
    )
    .unwrap();

    pub static ref DRIFT_CORRECTIONS_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_drift_corrections_total",
            "Total number of drift corrections applied"
        ),
        &["app_type", "namespace", "resource_type"]
    )
    .unwrap();

    pub static ref BACKUP_OPERATIONS_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_backup_operations_total",
            "Total number of backup and restore operations"
        ),
        &["app_type", "operation", "result"]
    )
    .unwrap();

    pub static ref MANAGED_APPS: IntGaugeVec = prometheus::register_int_gauge_vec!(
        Opts::new(
            "servarr_operator_managed_apps",
            "Number of managed apps per type and namespace"
        ),
        &["app_type", "namespace"]
    )
    .unwrap();

    pub static ref STACK_RECONCILE_TOTAL: IntCounterVec = prometheus::register_int_counter_vec!(
        Opts::new(
            "servarr_operator_stack_reconcile_total",
            "Total number of MediaStack reconciliations"
        ),
        &["result"]
    )
    .unwrap();

    pub static ref STACK_RECONCILE_DURATION: HistogramVec = prometheus::register_histogram_vec!(
        HistogramOpts::new(
            "servarr_operator_stack_reconcile_duration_seconds",
            "Duration of MediaStack reconciliations in seconds"
        ),
        &[]
    )
    .unwrap();

    pub static ref MANAGED_STACKS: IntGaugeVec = prometheus::register_int_gauge_vec!(
        Opts::new(
            "servarr_operator_managed_stacks",
            "Number of managed MediaStacks per namespace"
        ),
        &["namespace"]
    )
    .unwrap();
}

pub fn increment_reconcile_total(app_type: &str, result: &str) {
    RECONCILE_TOTAL.with_label_values(&[app_type, result]).inc();
}

pub fn observe_reconcile_duration(app_type: &str, duration_secs: f64) {
    RECONCILE_DURATION
        .with_label_values(&[app_type])
        .observe(duration_secs);
}

pub fn increment_drift_corrections(app_type: &str, namespace: &str, resource_type: &str) {
    DRIFT_CORRECTIONS_TOTAL
        .with_label_values(&[app_type, namespace, resource_type])
        .inc();
}

pub fn increment_backup_operations(app_type: &str, operation: &str, result: &str) {
    BACKUP_OPERATIONS_TOTAL
        .with_label_values(&[app_type, operation, result])
        .inc();
}

pub fn set_managed_apps(app_type: &str, namespace: &str, count: i64) {
    MANAGED_APPS
        .with_label_values(&[app_type, namespace])
        .set(count);
}

pub fn increment_stack_reconcile_total(result: &str) {
    STACK_RECONCILE_TOTAL.with_label_values(&[result]).inc();
}

pub fn observe_stack_reconcile_duration(duration_secs: f64) {
    STACK_RECONCILE_DURATION
        .with_label_values(&[] as &[&str])
        .observe(duration_secs);
}

pub fn set_managed_stacks(namespace: &str, count: i64) {
    MANAGED_STACKS.with_label_values(&[namespace]).set(count);
}
