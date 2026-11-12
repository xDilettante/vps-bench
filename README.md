# VPS Bench

[![CI](https://github.com/xDilettante/vps-bench/actions/workflows/ci.yml/badge.svg)](https://github.com/xDilettante/vps-bench/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/xDilettante/vps-bench)](https://github.com/xDilettante/vps-bench/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Rust CLI for benchmarking Linux VPS instances. Measures CPU, cryptography, RAM, disk and network, including latency and loss under TCP load. Displays live quality scales, keeps each stage's result in the terminal and saves a local JSON report.

| Раздел | Метрики |
|---|---|
| CPU и RAM | События/с в одном и нескольких потоках, CPU steal, чтение и запись RAM в МиБ/с |
| Crypto | Скорость SHA-256 и шифрования AES-256-GCM в МиБ/с |
| Диск | Последовательная скорость, случайные IOPS и задержки p50/p95/p99, опционально `fdatasync` |
| TCP | Входящая и исходящая скорость, повторные передачи и средний RTT при наличии данных |
| ICMP | RTT p50/p95/p99, потери и счётчики пакетов для каждого узла в простое, при исходящей и входящей нагрузке |

## Запуск из PowerShell по SSH

Установите инструменты на тестируемом VPS. Для Debian и Ubuntu подойдёт `sudo apt install curl sysbench openssl fio iperf3 iputils-ping`. Запустите `iperf3 -s` на отдельном сервере и разрешите доступ к его TCP-порту 5201 с тестируемых VPS.

Подставьте SSH-адрес тестируемого VPS, адрес сервера `iperf3` и имена или IP узлов для ICMP. Команда выбирает архив для Linux x86-64 или ARM64, проверяет SHA-256 и запускает утилиту в текущем SSH-каталоге.

```powershell
ssh -t user@vps-a.example.net 'bash -o pipefail -c ''curl -fsSL https://raw.githubusercontent.com/xDilettante/vps-bench/v0.8.0/run.sh | bash -s -- run --label vps-a --iperf-host bench.example.net --ping-target node1.example.net,node2.example.net --output vps-a.json'''
```

Для замера только RTT и потерь добавьте `--only latency` после `run`. Для второго VPS поменяйте SSH-адрес, метку и имя файла. Используйте те же узлы, сервер `iperf3` и параметры тестов.

## Демонстрация в WSL

Короткий прогон показывает CPU, Crypto, память и диск. В Ubuntu для него нужны `curl`, `sysbench`, `openssl` и `fio`.

```powershell
wsl -d Ubuntu -- bash -o pipefail -c 'cd ~ && curl -fsSL https://raw.githubusercontent.com/xDilettante/vps-bench/v0.8.0/run.sh | bash -s -- run --only cpu,crypto,memory,disk --seconds 5 --disk-mib 128'
```

В терминале шкала времени и цветная оценка замера показаны на разных строках. Полоса обновляется с каждым замером и меняет цвет при пересечении порога. Для CPU, Crypto, RAM и TCP используется скорость за интервал, для диска — скорость последовательного I/O или случайные IOPS с начала этапа, для ICMP — RTT последнего ответа. Для живой скорости TCP нужен `iperf3` с `--json-stream` (3.17 и новее).

После каждого этапа в терминале остаётся строка «Итог» с оценкой по финальным данным. Для ICMP выводятся RTT p95 и потери отдельно по каждому узлу. У быстрых тестов и TCP со старым `iperf3` первый доступный результат может быть итоговым. Живые оценки используют встроенные пороги VPS Bench. При перенаправлении stderr или с `--no-progress` выводится итоговый отчёт.

Для просмотра только RTT укажите `run --only latency --ping-target 127.0.0.1 --ping-count 40`. Живой экран, итог и сравнение показывают параметры каждого этапа. Диск определяется по каталогу `--disk-dir`. При доступности `findmnt` и `lsblk` выводятся том, файловая система и характеристики блочного устройства, видимые Linux. Флаг `ROTA` относится к устройству в гостевой ОС.

## Оценки первого запуска

При первом запуске `run` красная полоса означает «Плохо», жёлтая — «Норма», зелёная — «Хорошо». Под полосой указаны пороги для «Нормы» и «Хорошо» в единицах показателя. Более высокая скорость и более низкая задержка заполняют полосу вправо. Полная полоса соответствует уровню «Хорошо».

| Показатель | Норма | Хорошо |
|---|---:|---:|
| CPU sysbench, событий/с на поток | ≥ 500 | ≥ 1 200 |
| CPU steal | ≤ 5% | ≤ 1% |
| SHA-256, МиБ/с | ≥ 256 | ≥ 1 024 |
| AES-256-GCM, МиБ/с | ≥ 512 | ≥ 2 048 |
| Чтение RAM, МиБ/с | ≥ 5 120 | ≥ 12 288 |
| Запись RAM, МиБ/с | ≥ 4 096 | ≥ 10 240 |
| Диск, последовательный I/O, МиБ/с | ≥ 125 | ≥ 500 |
| Диск, случайный I/O 4 КиБ, IOPS | ≥ 1 000 | ≥ 5 000 |
| Задержка случайного I/O p50 / p95 / p99, мс | ≤ 2 / 5 / 10 | ≤ 0,5 / 1 / 2 |
| Запись 4 КиБ с `fdatasync`, IOPS | ≥ 100 | ≥ 1 000 |
| Задержка `fdatasync` p95 / p99, мс | ≤ 10 / 20 | ≤ 2 / 5 |
| Скорость TCP, Мбит/с | ≥ 90 | ≥ 900 |
| RTT до узла, мс | ≤ 100 | ≤ 30 |
| Потери ICMP | ≤ 1% | 0% |

Для многопоточного CPU границы умножаются на число потоков. Для RAM границы относятся к однопоточному тесту с буфером 64 МиБ. При другом буфере выводится измеренная скорость. Сетевые оценки относятся к маршруту до указанного узла в измеряемой фазе. Потери ICMP зависят от числа проб, при 100 запросах один пропуск даёт 1%.

Границы задаёт VPS Bench для быстрой оценки VPS общего назначения. При выборе масштаба CPU использованы опубликованные замеры sysbench с пределом 20 000. Порог последовательного I/O 125 МиБ/с соответствует базовой пропускной способности gp3 в условиях AWS. [Замеры CPU](https://lpc.events/event/18/contributions/1863/attachments/1634/3408/06-Nested%20CVM%20Performance.pdf), [спецификация EBS gp3](https://docs.aws.amazon.com/ebs/latest/userguide/general-purpose.html)

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

Чтобы увидеть разницу с эталоном сразу в итогах прогона, сохраните эталонный JSON и передайте его через `--baseline`.

```bash
./vps-bench run --only cpu,memory,disk --seconds 5 --disk-mib 128 -o reference.json
./vps-bench run --only cpu,memory,disk --seconds 5 --disk-mib 128 --baseline reference.json -o current.json
```

```bash
./vps-bench run --only cpu,memory --seconds 15 --memory-mib 128
./vps-bench run --only crypto --seconds 6
./vps-bench run --only disk --disk-dir /mnt/data --disk-mib 1024 --disk-sync
./vps-bench run --only latency --ping-target node1.example.net --ping-count 100
./vps-bench --color never compare vps-a.json vps-b.json
```

Без `--output` отчёт сохраняется как `vps-bench-<timestamp>.json`. Существующие файлы защищены от перезаписи. При частичном сбое отчёт сохраняется со статусом каждого теста, а код завершения равен 1. Цвет включается в терминале автоматически и учитывает `NO_COLOR`. Отчёты хранятся локально. Параметр `--seconds` задаёт длительность тестов с таймером, по умолчанию 15 секунд. Запись на диск ограничена объёмом тестового файла.

## Методика

- **ICMP.** По умолчанию 100 запросов на узел с интервалом 200 мс в каждой фазе. Узлы измеряются одновременно. RTT считается методом ближайшего ранга по полученным ответам, дубликаты исключаются. При полной потере указываются 100% и счётчики пакетов. Для сравнения выбирайте узлы, которые отвечают на ICMP. [iputils ping](https://github.com/iputils/iputils/blob/master/doc/ping.xml)
- **Нагрузка.** Утилита запускает по одному многопоточному TCP-тесту в каждом направлении. ICMP-пробы стартуют после первого подтверждённого интервала передачи. По умолчанию каждая нагруженная фаза длится 28 с. Число потоков задаёт `--streams`, число проб — `--ping-count`. При отсутствии `--iperf-host` выполняется фаза простоя. [iperf3](https://software.es.net/iperf/invoking.html)
- **CPU и память.** CPU использует профиль простых чисел `sysbench`. Память измеряется одним потоком с буфером 64 МиБ по умолчанию. Результат зависит от размера буфера и кеша CPU. [sysbench](https://github.com/akopytov/sysbench)
- **Crypto.** Стандартный прогон включает SHA-256 и AES-256-GCM в TLS-подобном режиме `-aead`. Каждый тест выполняет `--seconds` односекундных запусков `openssl speed` с блоками 16 КиБ и одним потоком. Живая шкала показывает скорость последнего интервала, итог — среднюю скорость в МиБ/с по реальному времени (`-elapsed`). Профиль и версия OpenSSL сохраняются в JSON. При недоступном OpenSSL этапы Crypto помечаются как пропущенные. [OpenSSL speed](https://docs.openssl.org/3.5/man1/openssl-speed/)
- **Диск.** `fio` работает с временным файлом на выбранном томе, по умолчанию в текущем каталоге. Основные тесты используют прямой I/O и глубину очереди 1. Запись ограничена размером файла. `--disk-sync` отдельно измеряет p95/p99 `fdatasync` после каждого блока 4 КиБ. [fio](https://fio.readthedocs.io/en/latest/fio_doc.html)

Для сравнения VPS повторяйте одинаковый профиль в разное время суток. TCP-нагрузка может заполнить канал, поэтому учитывайте объём трафика и текущую нагрузку на сервисы.

## Сборка

Требуется Rust 1.88 или новее. Для запуска замеров нужны GNU `timeout` и инструменты выбранных тестов в `PATH`.

```bash
cargo build --release --locked
cargo test --locked
```
