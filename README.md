# VPS Bench

[![CI](https://github.com/xDilettante/vps-bench/actions/workflows/ci.yml/badge.svg)](https://github.com/xDilettante/vps-bench/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/xDilettante/vps-bench)](https://github.com/xDilettante/vps-bench/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Rust CLI for benchmarking Linux VPS instances. Measures CPU, RAM, disk and network, including latency and loss under TCP load. Displays live measurements in the terminal and saves a local JSON report.

| Раздел | Метрики |
|---|---|
| CPU и RAM | События/с в одном и нескольких потоках, CPU steal, чтение и запись RAM в МиБ/с |
| Диск | Последовательная скорость, случайные IOPS и задержки p50/p95/p99, опционально `fdatasync` |
| TCP | Входящая и исходящая скорость, повторные передачи и средний RTT при наличии данных |
| ICMP | RTT p50/p95/p99, потери и счётчики пакетов для каждого узла в простое, при исходящей и входящей нагрузке |

## Запуск из PowerShell по SSH

Установите инструменты на тестируемом VPS. Для Debian и Ubuntu подойдёт `sudo apt install curl sysbench fio iperf3 iputils-ping`. Запустите `iperf3 -s` на отдельном сервере и разрешите доступ к его TCP-порту 5201 с тестируемых VPS.

Подставьте SSH-адрес тестируемого VPS, адрес сервера `iperf3` и имена или IP узлов для ICMP. Команда выбирает архив для Linux x86-64 или ARM64, проверяет SHA-256 и запускает утилиту в текущем SSH-каталоге.

```powershell
ssh -t user@vps-a.example.net 'bash -o pipefail -c ''curl -fsSL https://raw.githubusercontent.com/xDilettante/vps-bench/v0.5.0/run.sh | bash -s -- run --label vps-a --iperf-host bench.example.net --ping-target node1.example.net,node2.example.net --output vps-a.json'''
```

Для замера только RTT и потерь добавьте `--only latency` после `run`. Для второго VPS поменяйте SSH-адрес, метку и имя файла. Используйте те же узлы, сервер `iperf3` и параметры тестов.

## Демонстрация в WSL

Короткий прогон показывает разделы CPU, памяти и диска. В Ubuntu для него нужны `curl`, `sysbench` и `fio`.

```powershell
wsl -d Ubuntu -- bash -o pipefail -c 'cd ~ && curl -fsSL https://raw.githubusercontent.com/xDilettante/vps-bench/v0.5.0/run.sh | bash -s -- run --only cpu,memory,disk --seconds 5 --disk-mib 128'
```

В терминале видны текущий этап, шкала времени и промежуточные показатели. Для CPU, RAM и TCP выводятся значения за интервал, для диска — средние с начала этапа, для ICMP — RTT последнего ответа и счётчик ответов. Для промежуточной скорости TCP нужен `iperf3` с `--json-stream` (3.17 и новее). При перенаправлении stderr или с `--no-progress` остаётся итоговый отчёт.

Для просмотра только RTT укажите `run --only latency --ping-target 127.0.0.1 --ping-count 40`. Живой экран, итог и сравнение показывают параметры каждого этапа. Диск определяется по каталогу `--disk-dir`. При доступности `findmnt` и `lsblk` выводятся том, файловая система и характеристики блочного устройства, видимые Linux. Флаг `ROTA` относится к устройству в гостевой ОС.

## Запуск на Linux и сравнение

Скачайте архив и его файл `.sha256` со страницы [релизов](https://github.com/xDilettante/vps-bench/releases). Пример для x86-64:

```bash
sha256sum -c vps-bench-linux-x86_64.tar.gz.sha256
tar -xzf vps-bench-linux-x86_64.tar.gz
./vps-bench run --only latency --iperf-host bench.example.net \
  --ping-target node1.example.net --ping-target node2.example.net \
  --output vps-a.json
./vps-bench compare vps-a.json vps-b.json
```

`compare` читает оба отчёта с одной машины и показывает изменения относительно первого. Потери сравниваются в процентных пунктах. Сопоставляются одинаковые фазы для одного узла. Разный разрешённый IP, число проб или профиль нагрузки отмечаются как разные параметры. Отчёты 0.1.0 можно использовать для прежних тестов CPU, RAM, диска и TCP.

В цветной шкале центр соответствует эталону, левая сторона показывает ухудшение, правая — улучшение. Рост скорости и снижение задержки заполняют правую сторону. Рядом выводится точная разница и текстовая оценка. «Близко» означает изменение в пределах 5% для относительных величин или 0,1 процентного пункта для потерь и CPU steal. Полная сторона шкалы соответствует 50% или 5 процентным пунктам. При нулевом эталоне для остальных метрик видны направление и абсолютная разница.

Чтобы получить такую шкалу сразу в итогах прогона, сохраните эталонный JSON и передайте его через `--baseline`.

```bash
./vps-bench run --only cpu,memory,disk --seconds 5 --disk-mib 128 -o reference.json
./vps-bench run --only cpu,memory,disk --seconds 5 --disk-mib 128 --baseline reference.json -o current.json
```

```bash
./vps-bench run --only cpu,memory --seconds 15 --memory-mib 128
./vps-bench run --only disk --disk-dir /mnt/data --disk-mib 1024 --disk-sync
./vps-bench run --only latency --ping-target node1.example.net --ping-count 100
./vps-bench --color never compare vps-a.json vps-b.json
```

Без `--output` отчёт сохраняется как `vps-bench-<timestamp>.json`. Существующие файлы защищены от перезаписи. При частичном сбое отчёт сохраняется со статусом каждого теста, а код завершения равен 1. Цвет включается в терминале автоматически и учитывает `NO_COLOR`. Отчёты хранятся локально. Параметр `--seconds` задаёт длительность тестов с таймером, по умолчанию 15 секунд. Запись на диск ограничена объёмом тестового файла.

## Методика

- **ICMP.** По умолчанию 100 запросов на узел с интервалом 200 мс в каждой фазе. Узлы измеряются одновременно. RTT считается методом ближайшего ранга по полученным ответам, дубликаты исключаются. При полной потере указываются 100% и счётчики пакетов. Для сравнения выбирайте узлы, которые отвечают на ICMP. [iputils ping](https://github.com/iputils/iputils/blob/master/doc/ping.xml)
- **Нагрузка.** Утилита запускает по одному многопоточному TCP-тесту в каждом направлении. ICMP-пробы стартуют после первого подтверждённого интервала передачи. По умолчанию каждая нагруженная фаза длится 28 с. Число потоков задаёт `--streams`, число проб — `--ping-count`. При отсутствии `--iperf-host` выполняется фаза простоя. [iperf3](https://software.es.net/iperf/invoking.html)
- **CPU и память.** CPU использует профиль простых чисел `sysbench`. Память измеряется одним потоком с буфером 64 МиБ по умолчанию. Результат зависит от размера буфера и кеша CPU. [sysbench](https://github.com/akopytov/sysbench)
- **Диск.** `fio` работает с временным файлом на выбранном томе, по умолчанию в текущем каталоге. Основные тесты используют прямой I/O и глубину очереди 1. Запись ограничена размером файла. `--disk-sync` отдельно измеряет p95/p99 `fdatasync` после каждого блока 4 КиБ. [fio](https://fio.readthedocs.io/en/latest/fio_doc.html)

Для сравнения VPS повторяйте одинаковый профиль в разное время суток. TCP-нагрузка может заполнить канал, поэтому учитывайте объём трафика и текущую нагрузку на сервисы.

## Сборка

Требуется Rust 1.88 или новее. Для запуска замеров нужны GNU `timeout` и инструменты выбранных тестов в `PATH`.

```bash
cargo build --release --locked
cargo test --locked
```
