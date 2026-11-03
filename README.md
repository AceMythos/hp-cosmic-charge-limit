# hp-cosmic-charge-limit

Battery charge limiter for HP laptops on Pop!_OS / COSMIC, with a COSMIC panel applet.

Charging stops at a configurable percentage and resumes at a lower one. The
firmware only supports "charge" and "inhibit", so the percentage band is
enforced in software by a small root daemon that polls the battery.

```
stop at 80%  ─────────────►  inhibit charging
resume at 75% ◄─────────────  allow charging again
```

## Why

HP's `hp-wmi` kernel driver does not expose the charge-inhibit control on every
board. On the tested machine the firmware methods exist but are not wired up,
so this project calls them directly through `acpi_call`.

## How it works

The daemon uses HP WMI command `0x2B` through `/proc/acpi/call`:

| Action | ACPI call |
| --- | --- |
| Read | `\_SB.WMID.GBCO` |
| Inhibit charging | `\_SB.WMID.SBCO 0x0 0x5 0x0 0x0` |
| Restore auto | `\_SB.WMID.SBCO 0x0 0x0 0x0 0x0` |

`GBCO` returns a package whose first buffer byte is the current mode:
`0x00` = auto, `0x04` = inhibit.

Only modes `0` (auto) and `5` (inhibit) are ever written. Force-discharge and
the other HP modes are deliberately not used.

The daemon polls every `INTERVAL` seconds. At or above `STOP` it inhibits
charging; at or below `START` it restores auto. Runtime changes are saved in
`/var/lib/hp-charge-limit/state.json` and take precedence over
`/etc/hp-charge-limit.conf`.

The applet never touches ACPI. It talks to the daemon over a Unix socket at
`/run/hp-charge-limit.sock` (owner `root`, group from `SOCKET_GROUP`, mode
`0660`) using JSON lines.

## Tested on

- HP Victus 15-fb3xxx, board `8DD0`, BIOS `F.15`
- Pop!_OS 24.04, kernel `7.1.5-76070105-generic`, COSMIC / Wayland
- `acpi-call-dkms` 1.2.2, Secure Boot off
- Applet built against libcosmic rev `87ab817` (pinned in `applet/Cargo.lock`)

Other HP models may use different methods or modes. Check your own DSDT, or
test the calls manually before trusting this.

## Install

### 1. Dependencies

```bash
sudo apt install acpi-call-dkms
sudo modprobe acpi_call
echo acpi_call | sudo tee /etc/modules-load.d/hp-charge-limit.conf
```

If Secure Boot is on, the DKMS module must be signed / enrolled first.

### 2. Daemon

```bash
sudo install -Dm755 daemon/hp-charge-limit /usr/local/bin/hp-charge-limit
sudo install -Dm644 daemon/hp-charge-limit.conf /etc/hp-charge-limit.conf
sudo install -Dm644 daemon/hp-charge-limit.service /etc/systemd/system/hp-charge-limit.service
```

Edit `/etc/hp-charge-limit.conf` and set `SOCKET_GROUP` to your username (or a
group your user belongs to). Without it only root can use the socket.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now hp-charge-limit
systemctl status hp-charge-limit
```

### 3. Applet

```bash
cd applet
cargo build --release
sudo install -Dm755 target/release/hp-cosmic-charge-limit /usr/local/bin/hp-cosmic-charge-limit
install -Dm644 ../resources/io.github.AceMythos.HpCosmicChargeLimit.desktop \
  ~/.local/share/applications/io.github.AceMythos.HpCosmicChargeLimit.desktop
install -Dm644 ../resources/io.github.AceMythos.HpCosmicChargeLimit-symbolic.svg \
  ~/.local/share/icons/hicolor/scalable/status/io.github.AceMythos.HpCosmicChargeLimit-symbolic.svg
```

Then open **Settings → Desktop → Panel → Add applet** and pick **Charge Limit**.

## Usage

### Applet

Click the panel icon to open the popup:

- **Charge limit** toggle: turns the limiter on or off. Turning it off
  immediately restores firmware auto mode.
- **Stop at**: inhibit charging at or above this percentage.
- **Resume at**: allow charging again at or below this percentage.

Changes are saved when you release the slider.

### CLI

```bash
sudo hp-charge-limit status
sudo hp-charge-limit enable
sudo hp-charge-limit disable
sudo hp-charge-limit set 75 80        # START STOP
```

The socket API works without root for members of `SOCKET_GROUP`:

```bash
hp-charge-limit api '{"cmd":"status"}'
hp-charge-limit api '{"cmd":"enable","value":true}'
hp-charge-limit api '{"cmd":"set","start":75,"stop":80}'
```

### Socket protocol

One JSON object per line, one request per connection:

```json
{"cmd": "status"}
{"cmd": "enable", "value": true}
{"cmd": "set", "start": 75, "stop": 80}
```

Every reply is a JSON status object, or `{"ok": false, "error": "..."}`.

## Configuration

`/etc/hp-charge-limit.conf`:

| Key | Default | Meaning |
| --- | --- | --- |
| `START` | `75` | Resume charging at or below this percentage |
| `STOP` | `80` | Inhibit charging at or above this percentage |
| `INTERVAL` | `30` | Poll interval in seconds (minimum 5) |
| `ENABLED` | `true` | Initial limiter state |
| `SOCKET_GROUP` | empty | Group allowed to use the control socket |

`START` must be lower than `STOP`.

## Troubleshooting

```bash
systemctl status hp-charge-limit
journalctl -u hp-charge-limit -n 50 --no-pager
ls -l /run/hp-charge-limit.sock
```

- **Applet says "daemon not reachable"**: the service is not running, or your
  user is not in `SOCKET_GROUP`.
- **`/proc/acpi/call` missing**: `acpi_call` is not loaded or DKMS failed.
- **Firmware refuses the write**: the reply encodes an HP error code
  (`0x34` EC busy, `0x35` not allowed, `0x36` conflict, `0x37` EC error). The
  daemon logs the code.
- **Battery status still says Charging/Full while inhibited**: that is expected
  on some firmware. The `GBCO` mode (`inhibit`) is the source of truth.

## Notes and caveats

- This writes to firmware directly. Use at your own risk.
- `SBCO` contains an unbounded firmware wait on some boards. A firmware
  non-response can hang that call; a hard power-off recovers it and does not
  change any permanent setting.
- On some HP boards the inhibit state survives a reboot. The daemon restores
  auto mode when it is stopped.
- The percentage band is software-enforced. If the daemon is stopped while
  charging is inhibited, charging stays inhibited until the daemon restarts or
  the firmware is reset.

## Layout

```
daemon/     Python daemon, config, systemd unit
applet/     Rust COSMIC applet (libcosmic)
resources/  .desktop entry and symbolic icon
```

## License

MIT. See [LICENSE](LICENSE).
