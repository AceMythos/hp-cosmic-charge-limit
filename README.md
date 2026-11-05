<p align="center">
  <img src="resources/io.github.AceMythos.HpCosmicChargeLimit.svg" width="96" alt="Charge Limiter icon">
</p>

# hp-cosmic-charge-limit

Battery charge limiter for HP laptops on Pop!_OS and COSMIC. It includes a COSMIC panel applet.

The limiter stops charging at a chosen percentage. It resumes charging at a lower percentage. The firmware supports only "charge" and "inhibit". A small root daemon enforces the percentage band and polls the battery.

## Why

The HP `hp-wmi` kernel driver does not expose charge control on every board. This machine has the firmware methods. The driver does not connect them. This project calls the methods through `acpi_call`.

## How it works

The daemon uses HP WMI command `0x2B` through `/proc/acpi/call`.

| Action | ACPI call |
| --- | --- |
| Read state | `\_SB.WMID.GBCO` |
| Inhibit charging | `\_SB.WMID.SBCO 0x0 0x5 0x0 0x0` |
| Restore automatic charging | `\_SB.WMID.SBCO 0x0 0x0 0x0 0x0` |

`GBCO` returns a package. The first byte of the buffer shows the current state.

| Read value | Meaning |
| --- | --- |
| `0x00` | Automatic or idle |
| `0x01` | Charging |
| `0x02` | On battery |
| `0x03` | Low charge current |
| `0x04` | Charging inhibited |
| `0x05` | Inhibited by SHPM |
| `0x06` | Inhibited by STOE |
| `0xFF` | State unavailable |

The read values are not the same as the write modes. The daemon writes only mode `0` (automatic) and mode `5` (inhibit). It never writes force-discharge, SHPM, or STOE.

The daemon polls every `INTERVAL` seconds. At or above `STOP`, it inhibits charging. At or below `START`, it restores automatic charging. Runtime changes go to `/var/lib/hp-charge-limit/state.json`. These changes take priority over `/etc/hp-charge-limit.conf`.

The applet does not touch ACPI. It sends JSON lines to the daemon over `/run/hp-charge-limit.sock`. The socket owner is `root`. The group comes from `SOCKET_GROUP`. The mode is `0660`.

If the battery is above `STOP` when you connect AC, the limiter holds the current charge. It does not discharge the battery to `STOP`. The charge returns to `STOP` after a drain below `START` and the next charge cycle. A 30-second poll can let the charge rise by about one percent before the inhibit.

## Tested on

- HP Victus 15-fb3xxx, board `8DD0`, BIOS `F.15`
- Pop!_OS 24.04, kernel `7.1.5-76070105-generic`, COSMIC and Wayland
- `acpi-call-dkms` 1.2.2, Secure Boot off
- libcosmic rev `87ab817` (pinned in `applet/Cargo.lock`)

Other HP models can use different methods or modes. Check your DSDT. Test the calls manually before you trust this project.

## Install

### 1. Dependencies

```bash
sudo apt install acpi-call-dkms
sudo modprobe acpi_call
echo acpi_call | sudo tee /etc/modules-load.d/hp-charge-limit.conf
```

Pop!_OS does not support Secure Boot. If you use another distribution with Secure Boot, sign and enroll the DKMS module first.

### 2. Daemon

```bash
sudo install -Dm755 daemon/hp-charge-limit /usr/local/bin/hp-charge-limit
sudo install -Dm644 daemon/hp-charge-limit.conf /etc/hp-charge-limit.conf
sudo install -Dm644 daemon/hp-charge-limit.service /etc/systemd/system/hp-charge-limit.service
```

Edit `/etc/hp-charge-limit.conf`. Set `SOCKET_GROUP` to your user name or a group of your user. Without this value, only root can use the socket.

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
install -Dm644 ../resources/io.github.AceMythos.HpCosmicChargeLimit.svg \
  ~/.local/share/icons/hicolor/scalable/apps/io.github.AceMythos.HpCosmicChargeLimit.svg
install -Dm644 ../resources/io.github.AceMythos.HpCosmicChargeLimit-symbolic.svg \
  ~/.local/share/icons/hicolor/scalable/status/io.github.AceMythos.HpCosmicChargeLimit-symbolic.svg
```

Then open **Settings → Desktop → Panel → Add applet**. Select **Charge Limit**.

## Usage

### Applet

Click the panel icon. The popup shows these controls:

- **Charge limit** toggle: turns the limiter on or off. The off state restores firmware auto mode immediately.
- **Stop charging at**: inhibits charging at or above this percentage.
- **Resume charging at**: allows charging again at or below this percentage.

The daemon saves changes when you release a slider.

### CLI

```bash
sudo hp-charge-limit status
sudo hp-charge-limit enable
sudo hp-charge-limit disable
sudo hp-charge-limit set 75 80        # START STOP
```

The socket API works without root for members of `SOCKET_GROUP`.

```bash
hp-charge-limit api '{"cmd":"status"}'
hp-charge-limit api '{"cmd":"enable","value":true}'
hp-charge-limit api '{"cmd":"set","start":75,"stop":80}'
```

### Socket protocol

Send one JSON object per line. Use one request per connection.

```json
{"cmd": "status"}
{"cmd": "enable", "value": true}
{"cmd": "set", "start": 75, "stop": 80}
```

Every reply is a JSON status object or `{"ok": false, "error": "..."}`.

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

## Remove the limiter

1. Turn off the limiter. This step restores automatic charging.
   ```bash
   hp-charge-limit api '{"cmd":"enable","value":false}'
   ```
2. Stop and disable the service. The service also restores automatic charging when it stops.
   ```bash
   sudo systemctl disable --now hp-charge-limit
   ```
3. Remove the applet from the panel. Do this before you remove the applet binary.
4. Remove the installed files.
   ```bash
   sudo rm /usr/local/bin/hp-charge-limit
   sudo rm /usr/local/bin/hp-cosmic-charge-limit
   sudo rm /etc/systemd/system/hp-charge-limit.service
   sudo rm /etc/hp-charge-limit.conf
   sudo rm -r /var/lib/hp-charge-limit
   sudo rm /etc/modules-load.d/hp-charge-limit.conf

   rm ~/.local/share/applications/io.github.AceMythos.HpCosmicChargeLimit.desktop
   rm ~/.local/share/icons/hicolor/scalable/apps/io.github.AceMythos.HpCosmicChargeLimit.svg
   rm ~/.local/share/icons/hicolor/scalable/status/io.github.AceMythos.HpCosmicChargeLimit-symbolic.svg
   ```
5. Reload systemd.
   ```bash
   sudo systemctl daemon-reload
   ```
6. Optional: remove the kernel module if no other program uses it.
   ```bash
   sudo modprobe -r acpi_call
   sudo apt remove acpi-call-dkms
   ```

If charging stays limited after removal, run one manual command:

```bash
sudo sh -c 'echo "\_SB.WMID.SBCO 0x0 0x0 0x0 0x0" > /proc/acpi/call'
```

## Troubleshooting

```bash
systemctl status hp-charge-limit
journalctl -u hp-charge-limit -n 50 --no-pager
ls -l /run/hp-charge-limit.sock
```

- **The applet says "daemon not reachable".** The service is not running, or your user is not in `SOCKET_GROUP`.
- **`/proc/acpi/call` is missing.** `acpi_call` is not loaded, or DKMS failed.
- **The firmware refuses the write.** The reply contains an HP error code (`0x34` EC busy, `0x35` not allowed, `0x36` conflict, `0x37` EC error). The daemon logs the code.
- **The battery status says Charging or Full while `GBCO` reports inhibit.** Some firmware does this. The `GBCO` mode is the source of truth.

## Notes and caveats

- This project writes to firmware. Use it at your own risk.
- `SBCO` contains an unbounded wait loop on some boards. If the firmware does not answer, the call can hang. A hard power-off recovers the machine. No permanent setting changes.
- The firmware may keep the inhibit state across a reboot. The daemon restores automatic charging when it stops.
- The percentage band is software-enforced. If the daemon does not stop cleanly, charging can stay inhibited until the daemon starts again or you reset the firmware.

## Layout

```
daemon/     Python daemon, config, systemd unit
applet/     Rust COSMIC applet (libcosmic)
resources/  Desktop entry, app icon, symbolic panel icon
```

## License

MIT. See [LICENSE](LICENSE).
