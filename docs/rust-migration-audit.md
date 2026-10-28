# Calcite Core: аудит миграции C++ -> Rust

Дата аудита: 2026-10-28. Исходная ветка: `dev`, HEAD `27393d4`.
Рабочая ветка: `rust-migration-audit`.

## 1. Границы и исходное состояние

Подтверждено по дереву репозитория:

- Проект состоит из одного исполняемого файла `actualklasterkraft` и собирается через CMake.
- Исходный код использует C++23 modules (`.cppm`), Boost.Asio coroutines, Boost.JSON/containers и OpenSSL Crypto.
- В репозитории нет `Cargo.toml`, Rust-кода, тестов, benchmarks или GitHub Actions workflow.
- `README.md` описывает Minecraft Java Edition server; CMake требует CMake 3.30+.
- `src/main.cpp` открывает TCP-порт 25565, заранее загружает чанки в диапазоне x/z от -3 до 3 и запускает `boost::asio::io_context`.

Базовая проверка сборки:

```text
cmake -S . -B build-audit -G Ninja
```

Результат: конфигурация не выполнена, поскольку в окружении отсутствуют Ninja и C/C++ compiler. Это ограничение окружения; успешная сборка C++ до начала миграции не подтверждена.

## 2. Фактическая карта модулей

| Область | Подтверждённое назначение |
| --- | --- |
| `src/generic` | пулы, математические типы, сигналы/pub-sub, ошибки, formatters и coroutine completion tokens |
| `src/data` | VarInt/числа/строки/координаты, bitfields, NBT builder, block-state constants и большие prebuilt configuration payloads |
| `src/net/base` | acceptor, TCP transport, framing/read-write packet operations, disconnect helpers и state coroutine declarations |
| `src/net/statecoroutinesimpl` | Handshake, Status, Login, Configuration и Play state handlers |
| `src/net/play` | session, packet router, keepalive, motion, chat, player list, entity placement, initiation и chunk serialization |
| `src/world` | player pool/state, positions/rotations, chunk pool, paletted block storage, generation и storage hooks |
| `docs/configuration` | бинарные fixtures configuration packets 0..29 |
| `docs/chunk-structure` | описание формата chunk packet для protocol 775 |

## 3. Протокол и реализованное поведение

Подтверждено исходниками:

- Status response рекламирует Minecraft `26.1.2`, protocol `775` (`src/net/statecoroutinesimpl/status.cpp`).
- `src/data/prebuiltconfiguration.cppm` и fixtures также содержат configuration данные для линии 26.1.x.
- Состояния идут как `Handshake -> Status` либо `Handshake -> Login -> Configuration -> Play`; intent `Transfer` переводится в Login.
- Handshake разбирает protocol version, address, port и intent, но protocol version пока явно не проверяет.
- Status обрабатывает request `0x00`, ping `0x01` с 8-byte payload и отправляет pong.
- Login проверяет имя длиной 1..16, создаёт offline UUID через OpenSSL MD5 (`OfflinePlayer:<name>`), отправляет Login Success и ждёт Login Acknowledged.
- Configuration отправляет prebuilt registry/tag packets и обрабатывает finish acknowledgement; часть serverbound packets игнорируется.
- Play запускает loops для keepalive, motion, chat, player list и entity placement; клиент получает spawn/position/chunks.
- README заявляет status ping, один grass/air мир, Creative movement/break/place/cheat items и видимость движения/chat игроков. Блоки и items между игроками не синхронизируются.

Протокольные ограничения текущей реализации:

- Packet length VarInt ограничен тремя байтами.
- Нет видимого пути compression или encryption.
- Большинство packet IDs и payloads привязаны к protocol 775.
- `packetops` использует exact async reads и возвращает ошибки для слишком большого VarInt, короткого буфера и некорректного packet data.

## 4. Реализовано, частично реализовано и отсутствует

Реализовано в C++:

- TCP acceptor/transport и coroutine state machine.
- Базовые protocol primitives, packet framing, status ping, offline login.
- Игроки с pool на 256 слотов, EID, состояниями Alive/Dying/Dead, позициями и chat signals.
- Chunk pool на 16384 чанка, 24 секции для диапазона Y -64..319, paletted block storage, соседи и refcount.
- Базовая генерация grass/air с растениями, chunk serialization, heightmaps и light data.
- Chat, keepalive, motion, player list и entity placement handlers.

Частично реализовано или явно помечено:

- `src/world/chunkimpl/storage.cpp`: `load` всегда возвращает `false`, `store` ничего не делает; persistence отсутствует.
- `src/world/chunk.cppm`: изменение biomes помечено TODO; serialization использует hardcoded plains biome.
- `src/net/play/chunkserialization.cppm`: block entities помечены TODO.
- Configuration игнорирует часть serverbound packets; Client Information для Play не реализован.
- Version negotiation отсутствует, многие packet payloads hardcoded, автоматических тестов нет.

Не обнаружено в текущей ветке:

- Rust workspace и Rust implementation.
- Тесты, integration harness, benchmarks, CI и проверка реальным Minecraft client.
- Реализованные compression/encryption paths.
- Подтверждённая persistence backend.

## 5. Предлагаемая Rust-архитектура

Предлагаю workspace без циклических зависимостей:

```text
calcite-server  -> calcite-net -> calcite-protocol
       |                 |
       +----------> calcite-core
       +----------> calcite-world -> calcite-core
       +----------> calcite-entity -> calcite-core
```

- `calcite-core`: общие ошибки, координаты, UUID wrappers, pool/event interfaces и инварианты без сетевых зависимостей.
- `calcite-protocol`: bounded VarInt, packet frame, primitive codecs, protocol state/packet IDs и version-specific packet definitions.
- `calcite-entity`: Player/EID/state и владение игровыми сущностями.
- `calcite-world`: dimensions, chunk sections, palettes, block states, generation, light/heightmap и storage trait.
- `calcite-net`: Tokio TCP lifecycle, partial reads/writes, connection state machine и routing; зависит от protocol/core.
- `calcite-server`: config, tracing, runtime, accept loop и composition root.

Это разделение сохраняет protocol independent от gameplay rules и позволяет сначала сравнивать codec с C++ oracle.

Предлагаемый минимум зависимостей:

- `tokio` (runtime/net/time), `bytes` (framing buffers), `thiserror`, `anyhow`, `tracing`, `tracing-subscriber`.
- `serde`, `serde_json`, `toml` только для config/JSON formats.
- `flate2` при подтверждении необходимости compression; `fastnbt` после проверки API и лицензии.
- Поддерживаемая crypto crate для Minecraft login hash после отдельной проверки алгоритма, платформ и тест-векторов.
- Dev-dependencies: `proptest`, `criterion`; не добавлять их в production crates без тестовой необходимости.

## 6. Порядок миграции и зависимости

1. **Аудит/фиксация oracle** — сохранить C++ и packet fixtures; определить version matrix.
2. **Workspace/foundation** — Cargo workspace, MSRV, lint policy, core errors/math/UUID и CI skeleton.
3. **Protocol primitives** — VarInt, big-endian values, strings, XYZ/angles, bounded frame; golden и malformed-input tests.
4. **Network states** — Tokio transport, acceptor, Handshake/Status/Login, затем Configuration; dependency: шаг 3.
5. **World/entity data** — Player, pools, chunk palettes, generation, light/heightmap; dependency: core и protocol interfaces.
6. **Play handlers** — session/router, keepalive, motion, chat, player list, placement и chunk sync; dependency: шаги 4–5.
7. **Integration/stabilization** — server binary, compatibility tests, clippy/rustfmt, load benchmarks и comparison report.

Старый C++ target сохраняется до завершения сравнения; удаление или замена CMake target не входит в текущий этап.

## 7. Эквивалентность и тестирование

Для каждого перенесённого модуля критерий готовности должен включать:

- совпадение wire bytes с C++ для protocol 775 и fixtures из `docs/configuration`;
- property tests для VarInt, framing, partial reads, truncation, oversized lengths и unknown packet IDs;
- state transition tests для Handshake/Status/Login/Configuration/Play;
- deterministic tests для chunk palette, heightmap/light serialization и player state transitions;
- integration test с loopback TCP и bounded malformed input;
- `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features`, `cargo fmt --all -- --check`;
- benchmarks только до заявления о выигрыше производительности;
- реальный Minecraft client smoke test после появления runnable server path.

## 8. Риски и вопросы для решения

Основные риски:

- Protocol 775 и prebuilt 26.1.2 payloads могут быть неполной или быстро устаревающей version matrix.
- Отсутствие C++ baseline build/test в текущем окружении затрудняет byte-for-byte oracle.
- Недавние commits исправляли disconnect use-after-free; ownership и cancellation в Tokio требуют отдельного ревью.
- Большие generated registry/block-state данные усложнят перенос и review.
- Отсутствующие compression/encryption/persistence нельзя считать готовыми только из-за компиляции.

Нужны решения владельца проекта перед массовой реализацией:

1. Сохраняем ли поддержку только protocol 775 / Minecraft 26.1.2 на первом Rust milestone?
2. Должен ли C++ target собираться параллельно в CI, или достаточно сохранить его как oracle source?
3. Какой storage backend нужен первым: in-memory compatibility или конкретный persistent format?
4. Нужны ли compression/encryption в первом Rust milestone, если C++ их пока не реализует?
5. Предпочтительная MSRV и целевые платформы (Linux/Windows) для Rust workspace?

## 9. Распределение ролей

- **Tech Lead / Architect:** утвердить crate boundaries, version matrix, public APIs и порядок интеграции.
- **Rust Core Developer:** `calcite-core`, ошибки, math, pools и unit/property tests.
- **Protocol & Network Developer:** `calcite-protocol` и `calcite-net`, framing, state machine и malformed-input tests.
- **World & Gameplay Developer:** `calcite-world` и `calcite-entity`, chunk/player invariants и serialization tests.
- **QA / Independent Reviewer:** oracle comparisons, integration/regression/security review и acceptance checklist.
- **Performance Engineer:** Criterion benchmarks для framing, palette/chunk serialization и connection scaling; оптимизации только после измерений.

Массовая миграция намеренно не начата. Следующий шаг после согласования вопросов — утвердить version/MSRV scope и добавить только workspace foundation.
