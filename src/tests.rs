use super::bench::{cpu_rate, fio_metrics, iperf_metrics, memory_rate};
use super::host::{cpu_ticks, steal_percent};
use serde_json::json;

#[test]
fn parses_sysbench_cpu_and_memory_rates() {
    assert_eq!(
        cpu_rate("CPU speed:\n    events per second:  1984.20\n"),
        Ok(1984.20)
    );
    assert_eq!(
        memory_rate(
            "Total operations: 100 (100.00 per second)\n6400.00 MiB transferred (6400.00 MiB/sec)\n"
        ),
        Ok(6400.0)
    );
    assert!(memory_rate("Total operations: 0 (0.00 per second)").is_err());
}

#[test]
fn fio_uses_bytes_per_second_and_total_latency_in_nanoseconds() {
    let result = json!({"jobs": [{"error": 0, "read": {
        "io_bytes": 4096000,
        "bw_bytes": 4194304,
        "iops": 1024.0,
        "lat_ns": {"percentile": {"50.000000": 50000, "95.000000": 80000, "99.000000": 120000}}
    }}]});
    let metrics = fio_metrics(&result, "read", true).unwrap();
    assert_eq!(
        metrics.iter().map(|m| m.value).collect::<Vec<_>>(),
        [4.0, 1024.0, 0.05, 0.08, 0.12]
    );
    assert!(fio_metrics(&result, "write", true).is_err());
}

#[test]
fn iperf_uses_receiver_throughput_and_optional_sender_metrics() {
    let result = json!({"end": {
        "sum_received": {"bits_per_second": 980000000},
        "sum_sent": {"bits_per_second": 1000000000, "retransmits": 4},
        "streams": [{"sender": {"mean_rtt": 110}}, {"sender": {"mean_rtt": 130}}]
    }});
    let metrics = iperf_metrics(&result).unwrap();
    assert_eq!(
        metrics.iter().map(|m| m.value).collect::<Vec<_>>(),
        [980.0, 4.0, 0.12]
    );
    assert!(iperf_metrics(&json!({"error": "busy"})).is_err());
}

#[test]
fn steal_percentage_uses_deltas_and_excludes_guest_time() {
    let before = cpu_ticks("cpu 10 0 10 70 0 0 0 10 4 0").unwrap();
    let after = cpu_ticks("cpu 50 0 30 100 0 0 0 20 14 0").unwrap();
    assert_eq!(steal_percent(before, after), Some(10.0));
    assert_eq!(steal_percent(after, before), None);
    assert_eq!(steal_percent(after, after), None);
}

#[test]
fn comparisons_require_matching_workloads_but_allow_different_vcpu_counts() {
    use super::report::{Case, comparable_parameters};

    assert!(comparable_parameters(
        Case::CpuMulti,
        &json!({"seconds": 5, "threads": 2}),
        &json!({"seconds": 5, "threads": 4})
    ));
    assert!(!comparable_parameters(
        Case::NetDown,
        &json!({"host": "one", "seconds": 5}),
        &json!({"host": "two", "seconds": 5})
    ));
}

#[test]
fn ping_uses_unique_replies_and_nearest_rank_percentiles() {
    use super::latency::parse_ping;
    let output = "PING node.example (192.0.2.1) 56(84) bytes of data.\n\
        64 bytes from 192.0.2.1: icmp_seq=1 ttl=55 time=8.00 ms\n\
        64 bytes from 192.0.2.1: icmp_seq=1 ttl=55 time=99.00 ms (DUP!)\n\
        64 bytes from 192.0.2.1: icmp_seq=3 ttl=55 time=2.00 ms\n\
        4 packets transmitted, 2 received, +1 duplicates, 50% packet loss, time 600ms\n";
    let probe = parse_ping(output).unwrap();
    assert_eq!(probe.address.as_deref(), Some("192.0.2.1"));
    assert_eq!(
        probe.metrics.iter().map(|m| m.value).collect::<Vec<_>>(),
        [4.0, 2.0, 50.0, 2.0, 8.0, 8.0],
    );
}

#[test]
fn total_loss_has_no_rtt_and_load_needs_a_nonzero_live_interval() {
    use super::latency::{has_load, parse_ping};
    assert_eq!(
        parse_ping("3 packets transmitted, 0 received, 100% packet loss, time 600ms\n")
            .unwrap()
            .metrics
            .iter()
            .map(|m| m.value)
            .collect::<Vec<_>>(),
        [3.0, 0.0, 100.0]
    );
    assert!(parse_ping("ping: Name or service not known").is_err());
    assert!(!has_load("[  5] 0.00-1.00 sec 0.00 Bytes 0.00 bits/sec"));
    assert!(!has_load(
        "[  5] 0.00-6.00 sec 128 MBytes 1.00 Gbits/sec sender"
    ));
    assert!(has_load("[  5] 0.00-1.00 sec 128 MBytes 1.00 Gbits/sec"));
}

#[test]
fn live_sysbench_samples_are_separate_from_summary_metrics() {
    use super::bench::sysbench_interval;
    assert_eq!(
        sysbench_interval("[ 1s ] thds: 1 eps: 1408.20 lat (ms,95%): 0.72", false),
        Some(1408.20)
    );
    assert_eq!(
        sysbench_interval("[ 1s ]  47543.08 MiB/sec", true),
        Some(47543.08)
    );
    assert_eq!(sysbench_interval("events per second: 1123.25", false), None);
}

#[test]
fn fio_uses_the_last_complete_report_for_final_metrics() {
    use super::bench::last_fio_report;
    let output = r#"{"jobs":[{"read":{"bw_bytes":100}}]}
{"jobs":[{"read":{"bw_bytes":1000}}]}"#;
    assert_eq!(
        last_fio_report(output).unwrap()["jobs"][0]["read"]["bw_bytes"],
        1000
    );
    assert!(last_fio_report(&format!("{output}\n{{")).is_err());
}

#[test]
fn iperf_stream_keeps_interval_and_receiver_totals_distinct() {
    use super::bench::{iperf_interval_mbps, iperf_stream_report};
    let interval = r#"{"event":"interval","data":{"sum":{"bits_per_second":1250000000}}}"#;
    assert_eq!(iperf_interval_mbps(interval), Some(1250.0));
    let final_event = r#"{"event":"end","data":{"sum_received":{"bits_per_second":780000000}}}"#;
    let report = iperf_stream_report(&format!("{interval}\n{final_event}\n")).unwrap();
    assert_eq!(iperf_metrics(&report).unwrap()[0].value, 780.0);
    assert!(iperf_stream_report(interval).is_err());
}

#[test]
fn workload_descriptions_explain_actual_test_settings() {
    use super::report::Case;

    let cpu = Case::CpuSingle.workload(&json!({"threads": 1, "seconds": 15}));
    assert!(cpu.contains("простые числа до 20000"));
    assert!(cpu.contains("1 поток"));
    let disk = Case::DiskRandRead.workload(&json!({
        "block_kib": 4, "size_mib": 512, "iodepth": 1, "direct": true, "seconds": 15,
    }));
    assert!(disk.contains("блок 4 КиБ") && disk.contains("очередь 1"));
    assert!(disk.contains("прямой I/O") && disk.contains("файл 512 МиБ"));
    let tcp = Case::NetDownMulti.workload(&json!({
        "host": "::1", "port": 5201, "streams": 4, "seconds": 15,
    }));
    assert!(tcp.contains("[::1]:5201") && tcp.contains("4 потока"));
}

#[test]
fn disk_device_metadata_matches_the_measured_mount_by_device_number() {
    use super::disk::BlockDevice;

    let devices = json!({"blockdevices": [
        {"name":"sda","maj:min":"8:0","type":"disk","size":1000,"rota":true},
        {"name":"vdb1","maj:min":"252:17","type":"part","size":"2048","rota":0,"tran":null},
    ]});
    let device = BlockDevice::find(&devices, "252:17").unwrap();
    let value = serde_json::to_value(device).unwrap();
    assert_eq!(value["name"], "vdb1");
    assert_eq!(value["size_bytes"], 2048);
    assert_eq!(value["rotational"], false);
    assert!(BlockDevice::find(&devices, "0:83").is_none());
}
