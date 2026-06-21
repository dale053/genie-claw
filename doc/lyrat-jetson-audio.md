# LyraT / I2S2 Audio Frontend (Jetson Orin Nano)

GenieClaw can use an **ESP32-LyraT V4.3** wired to the Jetson Orin Nano 40-pin
header as its microphone frontend. The Jetson treats the LyraT as a passive
I2S source via the Tegra APE AHUB and surfaces it as `plughw:APE,0` for ALSA.

This document is the GenieClaw-specific install slice; the full step-by-step
bring-up (firmware, wiring, pinmux overlay, byte-exact verification) lives in
the hardware curriculum:

- [ai-hardware-engineer-roadmap — ESP32-LyraT I2S Mic on Jetson Orin Nano](https://github.com/ai-hpc/ai-hardware-engineer-roadmap/tree/main/Phase%204%20-%20Track%20B%20-%20Nvidia%20Jetson/5.%20Application%20Development/4.%20Multimedia/ESP32-LyraT-I2S-Mic-Jetson-Orin-Nano)

That guide is the source of truth for hardware setup. Sections below cover
only the GenieClaw integration.

## What this gets you

- LyraT onboard L+R microphones (3.5 cm apart) as `arecord -D plughw:APE,0`
- The existing `genie-core` voice loop unchanged — it already takes
  `audio_device` from config and shells out to `arecord` / `aplay`
- Boot-time AHUB route applied by `genie-audio.service` so no manual `amixer`
  after reboot

## Prerequisites on the Jetson

Done **before** running `make deploy`:

1. Flash LyraT with the `lyrat_jp4_passthrough` firmware (see the roadmap
   guide §9). Verify with PuTTY/screen on the LyraT's USB serial — you should
   see `JP4_PASSTHROUGH: rate=192000 B/s` every 2 s.
2. Wire 4 jumpers between LyraT JP4 and Jetson 40-pin header:
   `SCLK→12`, `LRCK→35`, `ASDOUT→38`, `GND→6`.
3. Enable the 40-pin I2S2 overlay: `sudo /opt/nvidia/jetson-io/jetson-io.py`,
   pick I2S2 from the 40-pin configurator, save, reboot.
4. Confirm controls exist: `amixer -c APE controls | grep I2S2 | wc -l`
   should report ≥18.

## GenieClaw side

The deploy pipeline now installs `/opt/geniepod/bin/genie-audio-init`, which
the existing `genie-audio.service` runs after `sound.target`. It sets:

```
ADMAIF1 Mux                    = I2S2
I2S2 codec master mode         = cbm-cfm   (external master drives BCLK + LRCK)
I2S2 codec frame mode          = i2s
I2S2 Sample Rate               = 48000
I2S2 Capture Audio Channels    = 2
I2S2 Capture Audio Bit Format  = 16
I2S2 Client  Channels          = 2
I2S2 Client  Bit Format        = 16
ADMAIF1 Capture Audio Channels = 2
ADMAIF1 Capture Client Channels= 2
```

Config (`/etc/geniepod/geniepod.toml`):

```toml
[core]
voice_enabled    = true
audio_device     = "auto"           # auto-detect picks plughw:APE,0 when the
                                    # ADMAIF1 -> I2S2 route is configured.
                                    # Or pin explicitly: "plughw:APE,0"
audio_sample_rate = 48000
wakeword_script  = ""               # empty = push-to-talk mode; set to
                                    # /opt/geniepod/bin/genie-wake-listen.py
                                    # once you want wake-word detection.
```

The `record_audio` helper in `genie-core` records mono. ALSA's `plughw:`
plugin downmixes the LyraT stereo to mono and resamples 48 kHz → 16 kHz for
Whisper, so no Rust changes are needed.

## Verify after deploy

On the Jetson:

```bash
# Route applied?
amixer -c APE cget name="ADMAIF1 Mux" | grep -E ': values'
# 4-second test capture
arecord -D plughw:APE,0 -c 1 -r 16000 -f S16_LE -d 4 /tmp/test.wav
sox /tmp/test.wav -n stat
# Should show non-zero RMS amplitude.
```

If the test WAV is silent, work back through the roadmap guide §14 ("Verified
test result — Jetson side") — that section enumerates the failure modes and
which `amixer` control or wire to inspect for each.

## Verified on real Jetson + LyraT V4.3 (2026-05-11)

End-to-end bring-up from cross-compile to live audio capture through the AHUB.

### Cross-compile + deploy from a Linux build host

From an x86_64 Ubuntu build VM (`tiny@tiny-virtual-machine`) with
`gcc-aarch64-linux-gnu` installed and `rustup target add aarch64-unknown-linux-gnu`:

```bash
make jetson      JETSON_HOST=<jetson> JETSON_USER=<user>
make deploy-binaries deploy-config deploy-systemd deploy-setup \
                 JETSON_HOST=<jetson> JETSON_USER=<user>
```

Five aarch64 release binaries (`genie-core` 4.3 MB, others 0.9 – 2.3 MB)
landed in `/opt/geniepod/bin/`. The audio init script and updated
`detect-audio-device.sh` landed alongside.

### Boot-time route setup ran cleanly

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now genie-audio.service
sudo systemctl status genie-audio.service --no-pager
```

```
● genie-audio.service - GeniePod Audio Subsystem (ALSA I2S + VAD + AEC)
     Active: active (exited) since 2026-05-10 21:31:09 EDT
    Process: 231827 ExecStart=/opt/geniepod/bin/genie-audio-init (code=exited, status=0/SUCCESS)
        CPU: 153ms
```

Service journal confirmed all ten `amixer cset` controls applied:

```
[genie-audio-init] configuring APE I2S2 → ADMAIF1 capture route
[genie-audio-init]   ADMAIF1 Mux                       = I2S2
[genie-audio-init]   I2S2 codec master mode            = cbm-cfm
[genie-audio-init]   I2S2 codec frame mode             = i2s
[genie-audio-init]   I2S2 Sample Rate                  = 48000
[genie-audio-init]   I2S2 Capture Audio Channels       = 2
[genie-audio-init]   I2S2 Capture Audio Bit Format     = 16
[genie-audio-init]   I2S2 Client Channels              = 2
[genie-audio-init]   I2S2 Client Bit Format            = 16
[genie-audio-init]   ADMAIF1 Capture Audio Channels    = 2
[genie-audio-init]   ADMAIF1 Capture Client Channels   = 2
[genie-audio-init] done
```

### Auto-detect resolves to the LyraT path

```bash
$ /opt/geniepod/bin/detect-audio-device.sh
plughw:APE,0
```

With `audio_device = "auto"` in `geniepod.toml`, `genie-core` shells out to
this script and consumes the result — so no manual config edit was needed
to switch from USB audio to the LyraT I2S2 path.

### Mono capture (current voice-loop call shape)

```bash
$ arecord -D plughw:APE,0 -c 1 -r 16000 -f S16_LE -d 5 /tmp/test.wav
$ sox /tmp/test.wav -n stat
Samples read:             80000
Maximum amplitude:     0.114044
RMS     amplitude:     0.011949
Mean    amplitude:    -0.000090
Rough   frequency:          183
Volume adjustment:        8.769
```

- `Samples read = 80000` = exactly `5 s × 16000 Hz × 1 ch` — **zero DMA
  underruns** through the on-the-fly `48 kHz/stereo → 16 kHz/mono` plughw
  conversion.
- `Mean amplitude ≈ 0` — no DC offset; codec biasing is clean.
- `Volume adjustment 8.769` → ~19 dB of headroom before clipping. If
  capture is too quiet, raise ES8388 PGA gain in firmware (preferred) or
  normalize downstream.

### Two-mic verification

Confirmed both LyraT capsules are alive by capturing stereo:

```bash
$ arecord -D plughw:APE,0 -c 2 -r 16000 -f S16_LE -d 5 /tmp/stereo.wav
# LEFT  channel:  Maximum 0.084412   RMS 0.008612
# RIGHT channel:  Maximum 0.074921   RMS 0.007612
```

L/R ratio ~1.13× — within normal onboard-mic sensitivity spread. This
matters because ALSA's `(L+R)/2` downmix (used implicitly when `arecord -c 1`
reads from a stereo source) acts as a passive broadside-pointing delay-and-sum
beamformer. With both channels carrying real signal, the mono capture
above gets ~3 dB of SNR gain over a single-mic capture for a speaker in
front of the device — for free, without any new code in the voice loop.

Active beamforming (steered DAS / GCC-PHAT DOA / MVDR) is not used in this
alpha. Both mics being verified-alive simply means the path is ready for
it whenever a future release wants to add it.

## Known wire-level rate quirk

The LyraT JP4 firmware (`examples/recorder/lyrat_jp4_passthrough/` in
`espressif/esp-adf`) configures ESP-IDF's I2S driver for 48 kHz, but on
ESP32-LyraT V4.3 + ESP-IDF v5.5.3 the actual LRCK frequency on the
wire is **24 kHz** (verified empirically: setting Jetson `I2S2 Sample
Rate` to 48 kHz produces 2× chipmunk playback; 24 kHz produces natural
pitch). Reason unknown — likely an APLL/MCLK divider constraint or
slot-width fallback inside the ESP32 I2S clock generator.

The current workaround is: tell the Jetson AHUB to expect 24 kHz.
`genie-audio-init` writes `I2S2 Sample Rate = 24000` for this reason.
Capture-side parameters in `[core]` (e.g. `audio_sample_rate = 16000`)
work fine — ALSA `plughw:APE,0` downsamples 24 kHz → 16 kHz cleanly.

Investigating the ESP-IDF clock setup so the LyraT actually emits 48 kHz
LRCK as configured remains future hardware/firmware work.

## Limitations / known gaps

- Capture only. TTS playback goes through the Jetson's default audio sink
  (HDMI / USB DAC / 3.5 mm), **not** the LyraT speakers. The LyraT's DAC
  side (`DSDIN` / `GPIO26`) is not driven by the current firmware.
- Wake-word is push-to-talk by default for the first alpha that supports
  LyraT. Set `wakeword_script` once you've validated push-to-talk works.
- No beamforming yet. The LyraT's L+R mics are 3.5 cm apart, which is
  below Espressif's recommended 4–6.5 cm range for their `MASE`/`esp_afe_doa`
  defaults; if/when beamforming lands in GenieClaw, `mic_distance` must be
  set to `0.035`, not the upstream defaults.
