# leasetrack

Track kilometer usage for your lease car. Calculates per-year usage, projects
end-of-year and end-of-contract totals, and warns when you are on track to
exceed your allowed kilometers.

The project is a Cargo workspace with four crates:

```
leasetrack/
├── core/    — shared business logic (library)
├── cli/     — command-line interface
├── api/     — REST API server
└── tui/     — terminal user interface
```

---

## core

The shared library used by all other crates. Contains:

- Data types: `LeaseConfig`, `KmRecord`, `LeaseData`, `ReportData`, `YearStats`
- `load_data` / `save_data` — reads and writes `~/.config/leasetrack.json`
- `add_record` — validates and inserts an odometer reading
- `compute_report_data` — computes per-year stats and end-of-lease projections
- `compute_year_stats` — per-year driven km using linear interpolation

The data file location can be overridden with the `LEASETRACK_DATA_FILE`
environment variable.

No async, no external services — pure local file I/O.

---

## cli

A command-line tool that reads and writes the local data file directly via
`leasetrack-core`.

### Build & run

```bash
cargo build -p leasetrack-cli
cargo run -p leasetrack-cli -- --help
```

### Commands

| Command | Description |
|---|---|
| `leasetrack init` | Interactive setup: creates the lease config |
| `leasetrack record <odometer>` | Add an odometer reading (optional `--date YYYY-MM-DD`) |
| `leasetrack report` | Print a per-year usage table with projections |
| `leasetrack graph` | Print a horizontal ASCII bar chart per lease year |
| `leasetrack list` | List all recorded odometer readings |

---

## api

An HTTP REST API server built with [Axum](https://github.com/tokio-rs/axum).
Shares `leasetrack-core` with the CLI.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `PORT` | `3000` | Port to listen on |
| `API_KEY` | *(unset)* | Legacy single-user key. Only consulted when no users are registered |
| `CORS_ORIGINS` | *(unset)* | `*` for permissive, comma-separated origins, or unset to deny |
| `APP_ENV` | `development` | Set to `production` to mark session cookies `Secure` (HTTPS only) |
| `APP_BASE_URL` | `http://localhost:3000` | Public URL used to build links in outgoing email |
| `LEASETRACK_DATA_DIR` | `~/.config` | Directory holding the per-user data files |
| `LEASETRACK_DATA_FILE` | `~/.config/leasetrack.json` | Shared data file (CLI, and the API in legacy mode) |
| `LEASETRACK_USERS_FILE` | `~/.config/leasetrack-users.json` | Registered users and their API keys |

### Authentication and data isolation

Each registered user gets their own data file, `leasetrack-<email>.json`, in
`LEASETRACK_DATA_DIR`. The JSON API and the web dashboard read and write that
same per-user file, so the two views always agree.

Authenticate with the key that was emailed at registration:

```bash
curl -H "X-Api-Key: <your key>" http://localhost:3000/list
```

Once any user is registered, a valid key is required — requests without one get
`401`. If no users exist the server falls back to the legacy single-tenant
modes: `API_KEY` if it is set, otherwise fully open for local development. Both
fall back to the shared `LEASETRACK_DATA_FILE`. `/health` is always public.

### Build & run

```bash
cargo build -p leasetrack-api
cargo run -p leasetrack-api
```

### Endpoints

All endpoints except `/health` act on the authenticated user's own data.

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check — returns `{"status":"ok"}` |
| `POST` | `/init` | Create or overwrite the lease config |
| `POST` | `/record` | Add an odometer reading (`odometer` required, `date` optional) |
| `GET` | `/report` | Full report with per-year stats and projections |
| `GET` | `/graph` | Per-year km data for charting |
| `GET` | `/list` | Raw lease data (config + all records) |

A [Dockerfile](./Dockerfile) is included for containerised deployment.

### Web dashboard

The API also serves a browser-based dashboard at `/web` (login required).

| Variable | Default | Description |
|---|---|---|
| `WEB_PASSWORD` | *(required)* | Password for the web dashboard |

The dashboard is a single-page HTML interface with:

- **Lease Info** — displays all lease configuration and running totals. An **Edit** button reveals editable fields and a **Save** button; a **Cancel** button returns to the read-only view without saving.
- **Record Odometer** — form to add a new odometer reading with a date picker.
- **Per-year graph** — horizontal bar chart showing km driven per lease year against the allowed amount.
- **Projections** — end-of-year and end-of-lease km estimates based on the current daily rate.
- **Records** — table of all recorded odometer readings with per-entry deltas.

---

## tui

A terminal user interface built with [Ratatui](https://ratatui.rs) that talks
to a running `leasetrack-api` instance over HTTP. No local file access — the
API is the only data source.

### Build & run

```bash
cargo build -p leasetrack-tui
cargo run -p leasetrack-tui
```

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `LEASETRACK_API_URL` | `https://leasetrack.apps.gertjanassies.dev` | Base URL of the API |
| `LEASETRACK_API_KEY` | *(required)* | API key — the app exits on startup if not set |

```bash
export LEASETRACK_API_KEY=your_key_here
cargo run -p leasetrack-tui
```

### Layout

```
┌─── Car Info ───────────────┬─── Records ──────────────────────┐
│  Car name                  │  Date        Odometer      Delta  │
│  Lease period              │  2026-08-04   28,542        +74   │
│  Allowed km/yr & total     │  ...                    ↑↓ scroll │
│  Last recorded odometer    │                                   │
├─── Graph ──────────────────┤                                   │
│  Yr1   12,779 ████████░░   │                                   │
│  Yr2   13,350 █████████░   │                                   │
│  Yr3    2,411 ██░░░░░░░░   │                                   │
├─── Projections ────────────┤                                   │
│  End yr 3:  12,124 km      │                                   │
│  End lease: 97,800 km      │                                   │
└────────────────────────────┴───────────────────────────────────┘
 leasetrack-tui  [r] record  [q] quit
```

### Key bindings

| Key | Action |
|---|---|
| `r` | Open record dialog (enter odometer + date, Tab to switch fields, Enter to save, Esc to cancel) |
| `q` | Quit |
| `↑` / `↓` or `k` / `j` | Scroll the records list |
