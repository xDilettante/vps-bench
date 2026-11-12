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
fn openssl_summary_requires_the_requested_block_size_and_algorithm() {
    use super::bench::openssl_rate;

    assert_eq!(
        openssl_rate("+H:16384\n+F:25:sha256:1048576000.00\n", "sha256"),
        Ok(1000.0)
    );
    assert_eq!(
        openssl_rate("+H:16384\n+F:24:SHA2-256:1048576000.00\n", "sha256"),
        Ok(1000.0)
    );
    for name in ["AES-256-GCM", "aes-256-gcm", "id-aes256-GCM"] {
        let output = format!("+H:16384\n+F:25:{name}:2147483648.00\n");
        assert_eq!(openssl_rate(&output, "aes-256-gcm"), Ok(2048.0));
    }
    for output in [
        "+F:25:sha256:1048576000.00\n",
        "+H:8192\n+F:25:sha256:1048576000.00\n",
        "+H:16384:8192\n+F:25:sha256:1048576000.00:524288000.00\n",
        "+H:16384\n+F:25:sha512:1048576000.00\n",
        "+H:16384\n+F:25:sha256:NaN\n",
        "+H:16384\n+F:25:sha256:0.00\n",
    ] {
        assert!(openssl_rate(output, "sha256").is_err(), "{output}");
    }
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

fn p95_measurement(
    test: super::report::Case,
    target: &str,
    address: Option<&str>,
    rtt: Option<f64>,
) -> super::report::Measurement {
    use super::bench::{Metric, MetricKind::RttP95Ms};
    use super::report::{Case, Measurement, Outcome};

    let mut parameters = json!({"count": 100, "interval_ms": 200, "resolved_address": address});
    if matches!(test, Case::PingUpload | Case::PingDownload) {
        parameters["load"] =
            json!({"host": "load.example", "port": 5201, "streams": 4, "seconds": 28});
    }
    let metrics = rtt
        .into_iter()
        .map(|value| Metric::new(RttP95Ms, value))
        .collect();
    Measurement::new(
        test,
        Some(target.into()),
        parameters,
        Outcome::completed(metrics),
    )
}

#[test]
fn rtt_p95_load_pairs_each_direction_with_its_own_target() {
    use super::latency::{LoadRttP95, annotate_rtt_p95_load};
    use super::report::Case::*;

    let address = Some("2001:db8::1");
    let mut rows = [
        p95_measurement(PingUpload, "b", address, Some(100.0)),
        p95_measurement(PingIdle, "a", address, Some(8.0)),
        p95_measurement(PingDownload, "a", address, Some(6.0)),
        p95_measurement(PingIdle, "b", address, Some(10.0)),
        p95_measurement(PingUpload, "a", Some("2001:0db8::1"), Some(74.0)),
    ];
    annotate_rtt_p95_load(&mut rows);
    assert_eq!(rows[0].rtt_p95_load.as_ref().unwrap().delta_ms, Some(90.0));
    assert_eq!(rows[2].rtt_p95_load.as_ref().unwrap().delta_ms, Some(-2.0));
    assert_eq!(
        rows[4].rtt_p95_load,
        Some(LoadRttP95 {
            idle_ms: Some(8.0),
            loaded_ms: Some(74.0),
            delta_ms: Some(66.0),
            reason: None,
        })
    );
    assert!(rows[1].rtt_p95_load.is_none());
}

#[test]
fn rtt_p95_load_keeps_missing_or_unpaired_values_explicit() {
    use super::latency::annotate_rtt_p95_load;
    use super::report::Case::*;

    let address = Some("192.0.2.1");
    let mut rows = [
        p95_measurement(PingIdle, "a", address, Some(10.0)),
        p95_measurement(PingUpload, "a", Some("192.0.2.2"), Some(20.0)),
        p95_measurement(PingDownload, "a", address, None),
        p95_measurement(PingUpload, "b", address, Some(25.0)),
        p95_measurement(PingDownload, "a", address, Some(20.0)),
        p95_measurement(PingUpload, "a", None, Some(20.0)),
        p95_measurement(PingUpload, "a", address, Some(30.0)),
    ];
    rows[4].parameters["count"] = json!(50);
    rows[6].parameters.as_object_mut().unwrap().remove("load");
    annotate_rtt_p95_load(&mut rows);

    for (index, reason) in [
        (1, "Разные IP узла"),
        (2, "Нет p95 под нагрузкой"),
        (3, "Нет p95 в простое"),
        (4, "Разные параметры ICMP"),
        (5, "IP узла неизвестен"),
    ] {
        let result = rows[index].rtt_p95_load.as_ref().unwrap();
        assert_eq!(result.delta_ms, None);
        assert_eq!(result.reason.as_deref(), Some(reason));
    }
    assert_eq!(rows[1].rtt_p95_load.as_ref().unwrap().loaded_ms, Some(20.0));
    assert!(rows[6].rtt_p95_load.is_none());
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
fn stability_parses_timestamps_and_keeps_zero_throughput() {
    use super::stability::{CpuSample, parse_sample};

    assert_eq!(
        parse_sample("[ 299s ] thds: 1 eps: 0.00 lat (ms,95%): 0.00"),
        Some(CpuSample {
            second: 299,
            events_per_second: 0.0,
        })
    );
    for line in [
        "events per second: 700.00",
        "[ 0s ] thds: 1 eps: 600.00",
        "[ bad ] thds: 1 eps: 600.00",
        "[ 60s ] thds: 1 eps: NaN",
    ] {
        assert_eq!(parse_sample(line), None);
    }
}

#[test]
fn stability_uses_observed_seconds_in_each_minute_and_nearest_rank_p10() {
    use super::stability::{CpuSample, summarize};

    let samples: Vec<_> = [
        (1, 1000.0),
        (60, 1200.0),
        (61, 2000.0),
        (240, 1600.0),
        (241, 800.0),
        (299, 600.0),
    ]
    .map(|(second, events_per_second)| CpuSample {
        second,
        events_per_second,
    })
    .into();
    let (metrics, series) = summarize(samples.clone(), 300).unwrap();
    let values: Vec<_> = metrics.iter().map(|metric| metric.value).collect();
    assert_eq!(values[..4], [1100.0, 600.0, 1100.0, 700.0]);
    assert!((values[4] - (-400.0 / 11.0)).abs() < 1e-10);
    assert_eq!(series.samples, samples);
    assert_eq!(series.first_minute_samples, 2);
    assert_eq!(series.last_minute_samples, 2);
    assert!(summarize(samples[..4].to_vec(), 300).is_err());
}

#[test]
fn stability_graph_keeps_elapsed_time_and_marks_missing_buckets() {
    use super::stability::{CpuSample, sparkline};

    let samples =
        [(1, 80.0), (2, 120.0), (21, 200.0)].map(|(second, events_per_second)| CpuSample {
            second,
            events_per_second,
        });
    assert_eq!(sparkline(&samples, 300), "График · ▁·█ · по 10 с · 100–200");
    assert_eq!(
        sparkline(&samples[..1], 600),
        "График · ▄ · по 20 с · 80–80"
    );
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

#[test]
fn assessment_points_toward_improvement_for_rates_and_delays() {
    use super::bench::MetricKind;
    use super::report::assessment;

    let faster = assessment(MetricKind::MibPerSecond, 100.0, 120.0).content();
    let lower_rtt = assessment(MetricKind::RttP95Ms, 20.0, 10.0).content();
    let slower = assessment(MetricKind::Iops, 100.0, 80.0).content();
    assert_eq!(faster, "─────│██───\nЛучше +20.00%");
    assert_eq!(lower_rtt, "─────│█████\nЛучше -50.00%");
    assert_eq!(slower, "───██│─────\nХуже -20.00%");
}

#[test]
fn assessment_handles_noise_band_and_zero_baselines() {
    use super::bench::MetricKind;
    use super::report::assessment;

    assert_eq!(
        assessment(MetricKind::EventsPerSecond, 100.0, 103.0).content(),
        "─────│─────\nБлизко +3.00%"
    );
    assert_eq!(
        assessment(MetricKind::PacketLossPercent, 0.0, 1.0).content(),
        "────█│─────\nХуже +1.000 п.п."
    );
    assert!(
        assessment(MetricKind::TcpRetransmits, 0.0, 2.0)
            .content()
            .contains("Хуже +2.000")
    );
    assert_eq!(
        assessment(MetricKind::PacketsSent, 100.0, 120.0).content(),
        "—"
    );
}

#[test]
fn first_run_scale_marks_cpu_boundaries_and_accounts_for_workers() {
    use super::bench::{Metric, MetricKind::EventsPerSecond};
    use super::quality::{rating, rating_cell};
    use super::report::Case;

    let single = json!({"threads": 1});
    for (value, filled, label, ansi) in [
        (0.0, 0, "Плохо", 31),
        (499.9, 3, "Плохо", 31),
        (500.0, 4, "Норма", 33),
        (1199.9, 9, "Норма", 33),
        (1200.0, 10, "Хорошо", 32),
        (2400.0, 10, "Хорошо", 32),
    ] {
        let metric = Metric::new(EventsPerSecond, value);
        let bar = format!("{}{} {label}", "█".repeat(filled), "░".repeat(10 - filled));
        let expected = format!("{bar}\n≥500 / ≥1200");
        assert_eq!(
            rating_cell(Case::CpuSingle, &single, &metric).content(),
            expected
        );
        let live = rating(Case::CpuSingle, &single, &metric).unwrap();
        assert_eq!(live.terminal(false), bar);
        assert_eq!(live.terminal(true), format!("\x1b[{ansi}m{bar}\x1b[0m"));
    }

    let rate = Metric::new(EventsPerSecond, 2000.0);
    let four = json!({"threads": 4});
    assert_eq!(
        rating_cell(Case::CpuMulti, &four, &rate).content(),
        "████░░░░░░ Норма\n≥2000 / ≥4800"
    );
    assert_eq!(
        rating_cell(Case::CpuMulti, &json!({}), &rate).content(),
        "—"
    );
}

#[test]
fn first_run_scale_rewards_lower_rtt_and_zero_loss() {
    use super::bench::{Metric, MetricKind::*};
    use super::quality::rating_cell;
    use super::report::Case;

    for (kind, value, expected) in [
        (RttP95Ms, 30.0, "██████████ Хорошо\n≤100 / ≤30"),
        (RttP95Ms, 100.0, "████░░░░░░ Норма\n≤100 / ≤30"),
        (RttP95Ms, 101.0, "███░░░░░░░ Плохо\n≤100 / ≤30"),
        (PacketLossPercent, 0.0, "██████████ Хорошо\n≤1 / ≤0"),
        (PacketLossPercent, 1.0, "████░░░░░░ Норма\n≤1 / ≤0"),
        (PacketLossPercent, 100.0, "░░░░░░░░░░ Плохо\n≤1 / ≤0"),
    ] {
        let metric = Metric::new(kind, value);
        assert_eq!(
            rating_cell(Case::PingUpload, &json!({}), &metric).content(),
            expected
        );
    }
}

#[test]
fn first_run_scale_uses_the_workload_and_keeps_diagnostic_counts_unrated() {
    use super::bench::{Metric, MetricKind::*};
    use super::quality::rating_cell;
    use super::report::Case;

    let speed = Metric::new(MibPerSecond, 512.0);
    assert_eq!(
        rating_cell(Case::DiskSeqWrite, &json!({}), &speed).content(),
        "██████████ Хорошо\n≥125 / ≥500"
    );
    let ram = json!({"block_mib": 64, "threads": 1});
    assert_eq!(
        rating_cell(Case::RamRead, &ram, &speed).content(),
        "░░░░░░░░░░ Плохо\n≥5120 / ≥12288"
    );
    let cached_ram = json!({"block_mib": 4, "threads": 1});
    assert_eq!(
        rating_cell(Case::RamRead, &cached_ram, &speed).content(),
        "—"
    );
    assert_eq!(
        rating_cell(
            Case::NetDown,
            &json!({}),
            &Metric::new(TcpRetransmits, 50.0)
        )
        .content(),
        "—"
    );
    assert_eq!(
        rating_cell(Case::PingIdle, &json!({}), &Metric::new(PacketsSent, 100.0)).content(),
        "—"
    );
}
