(ref_c37_118_implementation)=

```{meta}
:description: Byte-level implementation reference for IEEE C37.118.2-2024 version 3 and legacy version 2 frames.
```

# IEEE C37.118.2-2024 — implementation reference for WAMA

> Self-contained technical extraction of IEEE Std C37.118.2™-2024 (Synchrophasor Data Transfer for Power Systems, approved 6 June 2024, revision of C37.118.2-2011).
> Purpose: give a code-generating agent everything needed to build/parse/validate C37.118.2 messages **without opening the PDF**.
> The messages defined in the main clause are **version 3**. Version 2 (2011) framing is in §9; mixed-version conversion is in §10.

**Source PDF (SharePoint):**
https://eliagroup.sharepoint.com/sites/MCCSTopicGroups/Shared%20Documents/Product%20Cluster%20Grid%2C%20Asset%20%26%20System%2F02%20-%20Product%20Lines%2F06-Future_Product_Development%2FInitiatives%2FWAMA%2F10_WAMA_Data_Concepts%2FStandards%2FIEEE%20Std%20C37.118.2%E2%84%A2-2024.PDF

---

## 0. Golden rules for the agent

1. Treat this file as authoritative for framing; treat the PDF as authoritative if a detail is missing here.
2. **Byte order = big-endian** (network order) for all multi-byte integer and floating-point words.
3. All frames are transmitted **contiguously, no delimiters**, fixed layout.
4. **Never invent** field sizes, bit meanings, command codes, or error codes. If unknown, consult the PDF.
5. Floats are **IEEE 754 32-bit** unless stated otherwise.
6. Angles in 32-bit float are radians in range −π…+π. Angles in 16-bit int are radians × 10^4.
7. Do not hardcode channel counts, names, or scale factors — derive them from the configuration frame.

---

## 1. Message types (version 3)

Five message types; the "header" message from older versions is **deprecated**.

| Type | Purpose |
|---|---|
| Configuration – Capability | Full metadata: data types, calibration/scale factors, names, counts. |
| Configuration – Stream configuration | Per-stream configuration frame. |
| Periodic data | Real-time measurements at the reporting rate. |
| Discrete event data | Digital-only frame, sent on event (any time). |
| Command | Control/config messages sent **to** the PMU/PDC. |
| Error-response | Sent by source when a command is not understood/rejected. |

- **STREAM_ID** ties data ↔ configuration ↔ command ↔ error-response into one logical *data stream*.
- A PMU/PDC may emit multiple streams; each has a distinct STREAM_ID.

---

## 2. Common frame framework (all frame types)

Every frame is: `SYNC | FRAMESIZE | STREAM_ID | SOC | LEAP_BYTE | FRACSEC | <payload...> | CHK`

| # | Field | Bytes | Definition |
|---|---|---|---|
| 1 | SYNC | 2 | Sync + frame type + version (see §2.1). |
| 2 | FRAMESIZE | 2 | uint16, total bytes in frame incl. CHK. Max 65535. |
| 3 | STREAM_ID | 2 | uint16, 1…65534 (0 & 65535 reserved). Source stream (data/cfg/error) or destination stream (command). |
| 4 | SOC | 4 | uint32 second-of-century, UNIX epoch 01-Jan-1970. Leap seconds excluded. Rolls over 2106. |
| 5 | LEAP_BYTE | 1 | Leap second info (see §2.2). |
| 6 | FRACSEC | 3 | uint24 fraction of second (see §2.3). |
| … | payload | var | Depends on frame type. |
| last | CHK | 2 | CRC-CCITT (see §2.4). |

### 2.1 SYNC word (2 B)
- **Leading byte:** `0xAA`.
- **Second byte:**
  - Bits 7–4 = frame type (**version 3** codes):
    | Bits 7–4 | Frame type |
    |---|---|
    | `1000` | Periodic data frame |
    | `1001` | Discrete event data frame |
    | `1010` | Capability frame |
    | `1011` | Stream configuration frame |
    | `1100` | Command frame |
    | `1101` | Rename signals frame (command, optional) |
    | `1110` | Configure stream frame (command, optional) |
    | `1111` | Error-response frame |
  - Bits 3–0 = version number (binary 1–15). **Version 3 = `0011`** for messages in this revision.
- Note: bit 7 is set to 1 in v3 (was reserved/0 in v2) for backward compatibility.

### 2.2 LEAP_BYTE (1 B)
| Bit | Meaning |
|---|---|
| 7 | Reserved, 0. |
| 6 | Leap second direction: 0 = add, 1 = delete. |
| 5 | Leap second occurred (set for 24 h after event). |
| 4 | Leap second pending (set ≤12 h and ≥1 s before event). |
| 3–0 | Reserved, 0. (Were time-quality codes in v2.) |

### 2.3 FRACSEC (3 B)
- uint24. `FRACSEC = ROUND(fractional_second × TIME_BASE)`.
- Reconstruct time: `Time = SOC + FRACSEC / TIME_BASE`.
- TIME_BASE comes from the configuration frame.

### 2.4 CHK word (2 B)
- CRC-CCITT, 16-bit.
- Polynomial `x^16 + x^12 + x^5 + 1`, seed `0xFFFF` (−1), **no final XOR mask**.
- Computed over every byte from SYNC through the last payload byte inclusive.

---

## 3. Configuration frame (capability / stream configuration)

Header fields (SYNC…FRACSEC) as §2, then:

| # | Field | Bytes | Definition |
|---|---|---|---|
| 7 | CONT_IDX | 2 | Continuation index for fragmentation. `0`=single/unfragmented; `1`=first of series; `2…65534`=succeeding fragment; `0xFFFF`=last fragment. |
| 8 | TIME_BASE | 4 | Bits 31–24 reserved=0; bits 23–0 = uint24 subdivision of the second FRACSEC is based on. Must be integer multiple of the PMU data rate. |
| 9 | PDC_NAME | 1–256 | Name of composite stream (indexed UTF-8, see §3.3). |
| 10 | NUM_PMU | 2 | Number of PMUs included in the data frame. |

**Fields 11–34 repeat NUM_PMU times (per PMU):**

| # | Field | Bytes | Definition |
|---|---|---|---|
| 11 | PMU_NAME | 1–256 | UTF-8 indexed name. |
| 12 | PMU_ID | 2 | uint16, 1…65534. |
| 13 | PMU_VERSION | 2 | Bits 15–4 reserved=0; bits 3–0 = version from SYNC. |
| 14 | G_PMU_ID | 16 | Global PMU ID, RFC 4122 UUID, big-endian. |
| 15 | FORMAT | 2 | Data format bits (see §3.1). |
| 16 | PHNMR | 2 | Number of phasors. |
| 17 | ANNMR | 2 | Number of analog values. |
| 18 | FRNMR | 2 | Number of frequency signals. |
| 19 | DFDTNMR | 2 | Number of df/dt (ROCOF) signals. |
| 20 | DGNMR | 2 | Number of digital status words. |
| 21 | CHNAM | ≥2 each | Channel names, indexed UTF-8. Order: all phasors, then freqs, then ROCOFs, then analogs, then digitals. Digital names go bit0→bit15 per 16-bit word. |
| 22 | PHSCALE | 16 × PHNMR | Phasor scaling + flags (see §3.2). |
| 23 | FRSCALE | 8 × FRNMR | Frequency scale: 4 B magnitude M (float) + 4 B offset B (float); `X' = M·X + B`. Set 1.0/0.0 if already scaled float. |
| 24 | DFDTSCALE | 8 × DFDTNMR | ROCOF scaling, same layout as FRSCALE. |
| 25 | ANSCALE | 8 × ANNMR | Analog scaling, same layout as FRSCALE. |
| 26 | DIGUNIT | 4 × DGNMR | Two uint16 masks per digital word: normal-status mask (XOR→0 when normal) + valid-input mask. |
| 27 | PMU_LAT | 4 | Latitude, WGS84, float, −90…+90. Infinity = unspecified. |
| 28 | PMU_LON | 4 | Longitude, WGS84, float, −179.99999999…+180. Infinity = unspecified. |
| 29 | PMU_ELEV | 4 | Elevation m, WGS84, float. Infinity = unspecified. |
| 30 | PMUFLAG | 2 | Capability flags (see §3.4). |
| 31 | WINDOW | 4 | Measurement window length in µs, int32. −1 = unavailable. |
| 32 | GRP_DLY | 4 | Group delay in µs, int32. −1 = unavailable. |
| 33 | PMU_DATA_RATE | 2 | int16. >0 = frames/second; <0 = negative of seconds/frame. |
| 34 | CFGCNT | 2 | Config change count, incremented on each config change. 0 = factory default. |

**After the per-PMU block (once per frame):**

| Field | Bytes | Definition |
|---|---|---|
| STREAM_DATA_RATE | 2 | Composite stream rate (same encoding as PMU_DATA_RATE). May differ from member PMUs. |
| WAIT_TIME | 2 | PDC wait time in ms, uint16. |
| CHK | 2 | CRC. |

### 3.1 FORMAT word (field 15)
Bits 15–4 reserved=0.
| Bit | 0 | 1 |
|---|---|---|
| 3 FREQ/DFREQ | 16-bit integer | floating point |
| 2 ANALOG | 16-bit integer | floating point |
| 1 PHASOR format | 16-bit integer | floating point |
| 0 PHASOR encoding | real+imag (rectangular) | magnitude+angle (polar) |

### 3.2 PHSCALE (field 22, 16 B per phasor = four 4-B words)
- **Word 1:**
  - Bytes 0–1 (uint16 flags, bit0=LSB): modification flags — 1 up-sampled, 2 down-sampled, 3 magnitude filtered, 4 estimated magnitude, 5 estimated angle, 6 mag adjusted for calibration, 7 phase adjusted for calibration, 8 phase adjusted for offset, 9 pseudo-phasor, 10–14 reserved, 15 modification applied (type undefined). All 0 = no modification.
  - Byte 2 (phasor type): bits 7–4 reserved=0; bit 3: 0=voltage / 1=current; bits 2–0 component: `000` zero seq, `001` positive seq, `010` negative seq, `100` phase A, `101` phase B, `110` phase C, `011`/`111` reserved.
  - Byte 3: user-defined.
- **Word 2:** scale factor Y (float) → primary V/A. 1.0 if already scaled float.
- **Word 3:** angle offset θ (float, radians). 0.0 if already scaled float. Applied as `X' = Y·Xm·e^{j(φ−θ)}`.
- **Word 4:** voltage class (float, engineering units, e.g. 500000.0). 0.0 if unavailable.

### 3.3 Name fields (indexed UTF-8)
- Names are UTF-8 with a leading field index / length convention (Table 2 in the standard). Minimum 2 B per name. Do not assume fixed lengths — parse length-prefixed.

### 3.4 PMUFLAG word (field 30)
| Bit | Meaning |
|---|---|
| 15 | 1 = PMU rejects all config commands; 0 = accepts. |
| 14 | 1 = stream auto-starts on power up. |
| 13 | 1 = 50 Hz nominal; 0 = 60 Hz. |
| 12 | 1 = data attributes included in stream; 0 = not. |
| 11 | 1 = data available for retrieval (old data request); 0 = not. |
| 10–4 | Reserved=0. |
| 3–0 | Data filter class: 0 = P class, 1 = M class, 2–7 reserved, 8–15 user defined. |

### 3.5 Fragmentation
- If frame ≤ 65535 B, a single frame (CONT_IDX=0) may be used, or fragmentation using CONT_IDX.
- Fragments coordinated strictly by CONT_IDX (`1` first, incrementing, `0xFFFF` last).
- First-method detail (Annex I): virtual frame built per Table 3 with FRAMESIZE=65535, CONT_IDX=1, CHK over the whole virtual frame; then split into 65535-B wire fragments each carrying a 16-B header (fields 1–7). Non-last fragments = 16 B header + 65519 B data; last fragment FRAMESIZE=(16 + remaining), CONT_IDX=0xFFFF, carries CHK.

---

## 4. Periodic data frame

Header (SYNC…FRACSEC) as §2, then **fields 7–13 repeat NUM_PMU times**, then CHK.

| # | Field | Bytes | Definition |
|---|---|---|---|
| 7 | STAT_FLAG | 2 | Measurement status (see §4.1). |
| 8 | TIMEQUALITY | 2 | Per-PMU time quality (see §4.2). |
| 9 | PHASOR (+DAPHASOR) | (4 or 8)(+2) × PHNMR | Phasor estimates; +2 B optional data-attributes word per phasor if enabled. |
| 10 | FREQ (+DAFREQ) | (2 or 4)(+2) × FRNMR | Frequency; +2 B optional attributes. |
| 11 | DFREQ (+DADFREQ) | (2 or 4)(+2) × DFDTNMR | ROCOF; +2 B optional attributes. |
| 12 | ANALOG | (2 or 4) × ANNMR | Analog values. |
| 13 | DIGITAL | 2 × DGNMR | Digital status words (16 points each). |
| ++ | CHK | 2 | CRC. |

**Value encodings (per FORMAT word):**
- **Phasor, 16-bit int, rectangular:** real then imag, int16 −32767…+32767.
- **Phasor, 16-bit int, polar:** magnitude uint16 0…65535, then angle int16 = radians × 10^4 (−31416…+31416).
- **Phasor, 32-bit float, rectangular:** real then imag, engineering units.
- **Phasor, 32-bit float, polar:** magnitude (eng. units) then angle (radians −π…+π).
- **FREQ, 16-bit int:** deviation from nominal in mHz, int16 −32767…+32767.
- **FREQ, 32-bit float:** actual frequency in Hz.
- **DFREQ (ROCOF), 16-bit int:** Hz/s × 100, range −327.67…+327.67. Float form is Hz/s. Must match FREQ's format.

### 4.1 STAT_FLAG word (16-bit)
| Bits | Set by | Meaning |
|---|---|---|
| 15–14 | PMU | `00` good; `10` PMU test mode; `01` internal PMU error (use with caution); `11` reserved. |
| 13 | PMU | Time-sync uncertainty: 0 = ≤250 ns, 1 = >250 ns. |
| 12 | PDC | 1 = data had different timestamp; PDC reassigned current timestamp (formerly "Sort by Arrival"). |
| 11 | PDC | 1 = data bad / do not use (data still present). |
| 10 | PMU/PDC | PMU always 0; PDC sets 1 when substituting missing PMU data. |
| 9–0 | PMU | Reserved, 0. |
Data marked "PMU only" must not be modified downstream.

### 4.2 TIMEQUALITY word — version 3 stream (16-bit)
| Bits | Field | Meaning |
|---|---|---|
| 15 | Version indicator | 0 = version 3. |
| 14–12 | Multiplier | exponent n in 10^n. |
| 11–0 | Time sync uncertainty | uncertainty in ns (× multiplier). |
- All bits 14–0 = 0 ⇒ uncertainty < 0.5 ns (in sync). All 1 ⇒ ≥40.95 s or unknown.
- Set **only** by the measuring PMU; intermediate devices must not modify.

**Converted from a version 2 stream (bit 15 = 1):** bits 14–9 reserved=0; bits 8–6 = v2 STAT PMU time-quality; bits 5–4 reserved=0; bits 3–0 = v2 FRACSEC time-quality.

### 4.3 Data attributes (DAPHASOR/DAFREQ/DADFREQ, 2 B each, optional)
- Presence controlled by config (PMUFLAG bit 12 / data-attributes metadata).
- All-or-nothing: a PMU includes attribute words for all its phasor/freq/ROCOF measurements or none.

---

## 5. Discrete event data frame

Digital-only, SYNC type bits `1001`. Header as §2, then per PMU:

| # | Field | Bytes | Definition |
|---|---|---|---|
| 7 | DIGITAL | 2 × DGNMR | All digital signals, same sequence as periodic frame. |
| ++ | (repeat 7) | | Repeated NUM_PMU times. |
| ++ | CHK | 2 | CRC. |

- Sent immediately on event; reporting-rate rules do not apply.
- FRACSEC may be any integer < TIME_BASE (finer timing than periodic frames).

---

## 6. Command frame

Header as §2, then:

| # | Field | Bytes | Definition |
|---|---|---|---|
| 7 | CMD | 2 | Command code (see §6.1). |
| 8 | EXTFRAME | 0…65518 | Optional extended/user-defined 16-bit words. |
| 9 | CHK | 2 | CRC. |

- PMU/PDC must match STREAM_ID to a valid internal code before executing; command applies only to the addressed stream.

### 6.1 Command codes (CMD, 16-bit)
| Value (bits 15–0) | Meaning |
|---|---|
| `0000 0000 0001 0000` | Turn OFF data-frame transmission. |
| `0000 0000 0010 0000` | Turn ON data-frame transmission. |
| `0000 0000 0011 0000` | Reserved. |
| `0000 0000 0100 0000` | Send capability frame. |
| `0000 0000 0101 0000` | Reserved. |
| `0000 0000 0110 0000` | Send stream configuration frame. |
| `0000 0000 1000 0000` | Extended frame. |
| `0000 0000 xxxx xxxx` | (also) Old data request (optional); undesignated codes reserved. |
| `0000 yyyy xxxx xxxx` (yyyy≠0) | User-defined. |
| `zzzz xxxx xxxx xxxx` (zzzz≠0) | Reserved. |

### 6.2 Optional commands
- **Old data request (4.6.1.1):** destination requests stored frames by STREAM_ID + timestamp range; source replies with data frames if available (requires PMUFLAG bit 11).
- **Stream ID available**, **Rename signals** (SYNC `1101`), **Configure stream** (SYNC `1110`): all optional; a device need not implement them to be compliant.

---

## 7. Error-response frame

Sent when a command is invalid/unsupported. Header as §2, then:

| # | Field | Bytes | Definition |
|---|---|---|---|
| 7 | ERROR-RESPONSE-1 | 2 | Error code (see §7.1). |
| 8 | ERROR-RESPONSE-2 | 2 | Detail/sub-index; 0 unless specified. |
| 9 | CHK | 2 | CRC. |

PMUs configured to reject commands (PMUFLAG bit 15) need not send error-responses except when configured to (e.g., testing).

### 7.1 ERROR-RESPONSE-1 codes
| Value | Meaning |
|---|---|
| 0 | Reserved. |
| 1 | Could not interpret / rejected command. |
| 2 | Wrong STREAM_ID or PMU_ID. |
| 3–31 | Reserved. |
| 32 | Rename: count mismatch (NUM_PMU/PHNMR/ANNMR/FREQNMR/DFDTNMR). ER-2 = first mismatched field. |
| 33 | Rename: too many names. ER-2 = PMU number. |
| 34 | Rename: too few names. ER-2 = PMU number. |
| 35 | Rename: unacceptable characters. ER-2 = first offending name. |
| 36 | Rename: data stream is ON, command not accepted. |
| 37–63 | Reserved. |
| 64 | Configure stream: invalid TIME_BASE (no rate in Table 18 supported). |
| 65 | Configure stream: mismatched field (ER-2 = field sequence number). |

---

## 8. Communications & reporting rates (clause 5)

- Standard does **not** mandate a transport; any adequate channel (commonly TCP or UDP over IP). Large frames may be segmented across IP packets.

### 8.1 Required PMU reporting rates (frames per second)
| System | Required Fs values |
|---|---|
| 50 Hz | 10, 25, 50 |
| 60 Hz | 10, 12, 15, 20, 30, 60 |
- A compliant device supports at least one of these for its nominal frequency.
- Other rates permitted (e.g., 100, 120 fps; or <10 fps such as 1 fps).

### 8.2 Reporting times
- For N frames/s: frames numbered 0…N−1, frame 0 at UTC second rollover (FRACSEC 0), frame k at fractional time k/N.
- For rates <1/s: one report on the hour (xx:00:00), then evenly spaced.

---

## 9. Version 2 (2011) frame layouts — byte-level

Full v2 framing (Annex A). Use this to synthesize a byte-correct v2 fixture; there is no captured v2 stream to reference.

### 9.0 v2 and v3 structural differences (summary)
- **Message types (v2):** data, configuration (CFG-1, CFG-2, optional CFG-3), header (HDR), command. Header is deprecated in v3.
- **SYNC second byte (v2):** bit 7 reserved=0; bits 6–4 type: `000` data, `001` header, `010` CFG-1, `011` CFG-2, `101` CFG-3, `100` command. Bits 3–0 version: `0001`=v1 (C37.118-2005), `0010`=v2 (C37.118.2-2011).
- **Stream ID field is named IDCODE** (2 B) in v2 (= STREAM_ID role). Note IDCODE also appears again per-PMU in the config frame (source ID of each data block).
- **FRACSEC is 4 B in v2** (not 3 B): bits 31–24 = message time quality byte, bits 23–0 = 24-bit FRACSEC. In v3 the leap/time-quality bits were split into a separate 1-B LEAP_BYTE + a 3-B FRACSEC + per-PMU TIMEQUALITY.
- **v2 has exactly one FREQ and one ROCOF per PMU** (no FRNMR/DFDTNMR); v3 adds these counts.
- **v2 phasor scaling is PHUNIT (4 B/phasor)**; v3 uses PHSCALE (16 B/phasor). v2 analog scaling is ANUNIT (4 B); v3 uses ANSCALE (8 B).
- v2 has no G_PMU_ID, PMU_VERSION, PMU_LAT/LON/ELEV, WINDOW, GRP_DLY, PMU_DATA_RATE, WAIT_TIME, TIMEQUALITY, data-attributes. v2 has SVC_CLASS and FNOM that v3 folds into PMUFLAG.

### 9.1 v2 SYNC second-byte values (corrected — read carefully)

**Second byte = bit7 `0` + type(bits 6–4) + version nibble(bits 3–0).** For a genuine **version-2** (C37.118.2-2011) frame the version nibble is `0010`.

| Frame | Type bits 6–4 | **v2 second byte (nibble 0010)** | Annex A example (v1, nibble 0001) |
|---|---|---|---|
| Data | `000` | **0x02** | 0x01 |
| Header (HDR) | `001` | **0x12** | — |
| CFG-1 | `010` | **0x22** | 0x21 |
| CFG-2 | `011` | **0x32** | 0x31 |
| Command | `100` | **0x42** | 0x41 |
| CFG-3 | `101` | **0x52** | — |

First byte is always `0xAA`, so e.g. a v2 command SYNC word = `0xAA42`.

> **Conflict note (verified against Annex A):** Table A.1 normatively states version 2 = nibble `0010`. However the *concrete hex examples* in Table A.5 (`01`), Table A.8 (`21`/`31`), and Table A.13 (`AA41`) are explicitly labeled **"version 1" (IEEE Std C37.118-2005)** — they are illustrative carryovers from the 2005 edition, **not** the version-2 encoding. For version-2 devices/fixtures, follow the normative A.1 rule and use nibble `0010` → **0x02/0x12/0x22/0x32/0x42 (0x52 for CFG-3)**. Confirm the version nibble in the SYNC word of any received frame before assuming a version.

### 9.2 v2 common word: FRACSEC (4 B) + message time quality (Table A.2/A.3)
- Bits 31–24 = message time-quality byte:
  | Bit | Meaning |
  |---|---|
  | 7 | Reserved. |
  | 6 | Leap second direction (0 add, 1 delete). |
  | 5 | Leap second occurred (24 h). |
  | 4 | Leap second pending (≤60 s, ≥1 s before). |
  | 3–0 | Message time-quality code (MSG_TQ, Table A.3): `1111`(F)=clock failure/time unreliable … down to finer codes. |
- Bits 23–0 = uint24 FRACSEC; `fractional_second = FRACSEC / TIME_BASE`.

### 9.3 v2 data frame (Table A.4 / A.5)
Header: `SYNC(2) | FRAMESIZE(2) | IDCODE(2) | SOC(4) | FRACSEC(4)`, then **fields 6–11 repeat NUM_PMU times**, then CHK(2).

| # | Field | Bytes | Notes |
|---|---|---|---|
| 6 | STAT | 2 | v2 status word (see §9.3.1). |
| 7 | PHASORS | (4 or 8) × PHNMR | Same int/float encodings as v3 (§4). |
| 8 | FREQ | 2 or 4 | Deviation from nominal in mHz (int) or actual Hz (float). |
| 9 | DFREQ | 2 or 4 | ROCOF Hz/s ×100 (int) or Hz/s (float). |
| 10 | ANALOG | (2 or 4) × ANNMR | |
| 11 | DIGITAL | 2 × DGNMR | |
| ++ | CHK | 2 | CRC. |

#### 9.3.1 v2 STAT word (16-bit) — differs from v3 STAT_FLAG
| Bits | Meaning |
|---|---|
| 15–14 | Data error: `00` good; `01` PMU error (no info); `10` PMU test mode / absent-data inserted (do not use); `11` PMU error (do not use). |
| 13 | PMU sync: 0 = in sync with UTC-traceable source. |
| 12 | Data sorting: 0 = by timestamp, 1 = by arrival. |
| 11 | PMU trigger detected (0 = none). |
| 10 | Configuration change (set 1 min before change). |
| 9 | Data modified by post-processing. |
| 8–6 | PMU time quality (PMU_TQ, Table A.6). |
| 5–4 | Unlocked time: `00` <10 s; `01` 10–100 s; `10` 100–1000 s; `11` >1000 s. |
| 3–0 | Trigger reason: `0000` manual, `0001` mag low, `0010` mag high, `0011` phase angle diff, `0100` freq high/low, `0101` df/dt high, `0110` reserved, `0111` digital, `1000`–`1111` user. |

#### 9.3.2 PMU_TQ codes (STAT bits 8–6, Table A.6)
| Bits | Worst-case time error |
|---|---|
| `111` | >10 ms or unknown |
| `110` | <10 ms |
| `101` | <1 ms |
| `100` | <100 µs |
| `011` | <10 µs |
| `010` | <1 µs |
| `001` | <100 ns |
| `000` | Not used (code from previous profile) |

### 9.4 v2 configuration frame CFG-1 / CFG-2 (Table A.7 / A.8)
Header: `SYNC(2) | FRAMESIZE(2) | IDCODE(2) | SOC(4) | FRACSEC(4) | TIME_BASE(4) | NUM_PMU(2)`, then **fields 8–19 repeat NUM_PMU times**, then `DATA_RATE(2) | CHK(2)`.

| # | Field | Bytes | Notes |
|---|---|---|---|
| 8 | STN | 16 | Station name, **fixed 16 B ASCII** (v3 uses variable indexed UTF-8). |
| 9 | IDCODE | 2 | Per-PMU source ID. |
| 10 | FORMAT | 2 | Same bit layout as v3 FORMAT (bit3 FREQ/DFREQ, bit2 ANALOG, bit1 PHASOR fmt, bit0 PHASOR polar/rect). |
| 11 | PHNMR | 2 | Number of phasors. |
| 12 | ANNMR | 2 | Number of analogs. |
| 13 | DGNMR | 2 | Number of digital words. |
| 14 | CHNAM | 16 × (PHNMR+ANNMR+16×DGNMR) | **Fixed 16 B ASCII** per channel name; 16 digital names per digital word. |
| 15 | PHUNIT | 4 × PHNMR | MSB: 0=voltage,1=current; lower 24-bit = 10⁻⁵ V or A per bit (int scaling; ignored if float). |
| 16 | ANUNIT | 4 × ANNMR | MSB: 0=point-on-wave,1=rms,2=peak,5–64 reserved,65–255 user; lower 24-bit signed user scaling. |
| 17 | DIGUNIT | 4 × DGNMR | Two 16-bit masks per digital word (normal-status XOR mask + valid-input mask). |
| 18 | FNOM | 2 | Bit 0: 1=50 Hz, 0=60 Hz; bits 15–1 reserved. |
| 19 | CFGCNT | 2 | Config change count. |
| ++ | DATA_RATE | 2 | int16: >0 frames/s, <0 −seconds/frame. |
| ++ | CHK | 2 | CRC. |

- **SYNC 2nd byte (v2 nibble 0010):** CFG-1 = `0x22`, CFG-2 = `0x32`. (Annex A prints `0x21`/`0x31` — those are v1; see §9.1 conflict note.)
- **CFG-1** = capability (all data the device *can* report); **CFG-2** = data currently reported. Same layout.
- **CFG-3** (optional): variable/flexible framing with much of the same data plus fragmentation — see PDF Table A.9/A.10 if you must emit CFG-3; not required for v2 compliance.

### 9.5 v2 Command frame (Table A.13 / A.14)
`SYNC(2) | FRAMESIZE(2) | IDCODE(2) | SOC(4) | FRACSEC(4) | CMD(2) | EXTFRAME(0–65518) | CHK(2)` — v2 SYNC = `0xAA42` (Annex A prints `AA41` = v1; see §9.1).

| CMD (bits 15–0) | Meaning |
|---|---|
| `…0000 0000 0001` | Turn off data-frame transmission. |
| `…0000 0000 0010` | Turn on data-frame transmission. |
| `…0000 0000 0011` | Send HDR frame. |
| `…0000 0000 0100` | Send CFG-1 frame. |
| `…0000 0000 0101` | Send CFG-2 frame. |
| `…0000 0000 0110` | Send CFG-3 frame (optional). |
| `…0000 0000 1000` | Extended frame. |
| `0000 0000 xxxx xxxx` | Undesignated reserved. |
| `0000 yyyy xxxx xxxx` (yyyy≠0) | User-defined. |
| `zzzz xxxx xxxx xxxx` (zzzz≠0) | Reserved. |

---

## 10. Annex H — mixed-version conversion (byte-correct)

Normative-in-practice mapping for a PDC bridging v2 ↔ v3. Exact 1:1 conversion is not always possible; follow these field actions.

### 10.1 Version detection (H.1)
- v3 PDC → send **"Send capability" (0x0040)**. Correct reply ⇒ v3 PMU. No reply ⇒ send **Send CFG-1/CFG-2** (v2). Correct reply ⇒ v2 PMU.
- Always confirm by checking the **version nibble in the SYNC word** of the response.
- v2 PMUs do **not** accept remote configuration commands.

### 10.2 Command conversion

**v2 → v3 (Table H.1):**
| v2 command | v3 command |
|---|---|
| Turn off (0x0001) | Turn off (0x0010) |
| Turn on (0x0002) | Turn on (0x0020) |
| Send HDR (0x0003) | Not supported |
| Send CFG-1 (0x0004) | Send capability (0x0040) |
| Send CFG-2 (0x0005) | Send stream configuration (0x0060) |
| Send CFG-3 (0x0006) | Send stream configuration (0x0060) |
| Extended (0x0008) | (none in v2 context) |

**v3 → v2 (Table H.5):**
| v3 command | v2 command |
|---|---|
| Turn off (0x0010) | Turn off (0x0001) |
| Turn on (0x0020) | Turn on (0x0002) |
| Send capability (0x0040) | Send CFG-1 (0x0004) |
| Send stream configuration (0x0060) | Send CFG-2 (0x0005) **and** Send CFG-3 (0x0006)ᵃ |
| Extended (0x0080) / Old data request | Omit — not supported in v2 |

ᵃ CFG-3 optional in v2; PMU may not respond. Use both Send CFG-2 and Send CFG-3.

### 10.3 Config conversion v3 → v2 (Table H.2)
| v3 field | v2 field | Action |
|---|---|---|
| CONT_IDX | CONT_IDX | Recalculate CRC/size context; function unchanged. |
| TIME_BASE | TIME_BASE | No change. |
| PDC_NAME | — | Delete. |
| NUM_PMU | NUM_PMU | No change. |
| PMU_NAME | STN | No change (renamed; v2 STN is fixed 16 B ASCII). |
| PMU_ID | IDCODE | No change. |
| PMU_VERSION | — | Delete. |
| G_PMU_ID | — | Delete. |
| FORMAT | FORMAT | No change. |
| PHNMR | PHNMR | No change. |
| ANNMR | ANNMR | No change. |
| DGNMR | DGNMR | No change. |
| FRNMR | — | Delete (v2 has exactly one freq). |
| DFDTNMR | — | Delete (v2 has exactly one ROCOF). |
| CHNAM | CHNAM | No change (convert to fixed 16 B ASCII). |
| PHSCALE 16×PHNMR | PHSCALE 12×PHNMR | **Delete the 4th 4-B word (voltage class)** — not in v2. |
| FRSCALE | — | Delete. |
| DFDTSCALE | — | Delete. |
| ANSCALE | ANSCALE | No change. |
| DIGUNIT | DIGUNIT | No change. |
| PMU_LAT/LON/ELEV | PMU_LAT/LON/ELEV | No change. |
| PMUFLAG (2 B) | SVC_CLASS (1 B) | Bits 15–4 → 0; bits 3–0 ← incoming PMUFLAG bits 3–0. |
| WINDOW | WINDOW | No change. |
| GRP_DLY | GRP_DLY | No change. |
| PMU_DATA_RATE | — | Delete. |
| — | FNOM (2 B) | Set from incoming PMUFLAG bit 13 (50/60 Hz). |
| CFGCNT | CFGCNT | No change. |
| STREAM_DATA_RATE | DATA_RATE | Same value. |
| WAIT_TIME | — | Delete. |
| CHK | CHK | Recalculate. |

### 10.4 Data conversion v3 → v2 (Table H.3)
| v3 field | v2 field | Action |
|---|---|---|
| STAT_FLAG | STAT | See §10.5. |
| TIMEQUALITY | — | Delete. |
| PHASOR (+DAPHASOR) | PHASORS | Drop data attributes (set PMUFLAG bit 12=0). |
| FREQ (+DAFREQ) | FREQ | Drop data attributes. |
| DFREQ (+DADFREQ) | DFREQ | Drop data attributes. |
| ANALOG | ANALOG | No change. |
| DIGITAL | DIGITAL | No change. |
- v3 **discrete event** frames cannot be represented in a v2 output — archive only, do not forward.

### 10.5 STAT_FLAG (v3) → STAT (v2) (Table H.4)
| v2 STAT bits | v2 meaning | Action from v3 STAT_FLAG |
|---|---|---|
| 15–14 | Data error | Copy from v3 bits 15–14. |
| 13 | PMU sync | Copy from v3 bit 13. |
| 12 | Data sorting | Copy from v3 bit 12. |
| 11 | Trigger | Set 0 (not in v3). |
| 10 | Config change | Set 0 (not in v3). |
| 9 | Data modified | Set 0 if any incoming data-attribute indicates modification. |
| 8–6 | PMU time quality | Calculate from v3 TIMEQUALITY bits 14–0. |
| 5–4 | Unlocked time | Set 0 (not in v3). |
| 3–0 | Trigger reason | Set 0 (not in v3). |

### 10.6 Config conversion v2 → v3

**From CFG-3 (Table H.6)** — high-fidelity; most fields map directly:
| v2 field | v3 field | Action |
|---|---|---|
| CONT_IDX | CONT_IDX | Recalc. |
| TIME_BASE | TIME_BASE | No change. |
| — | PDC_NAME | Add. |
| NUM_PMU | NUM_PMU | No change. |
| STN | PMU_NAME | No change (renamed). |
| IDCODE | PMU_ID | No change. |
| — | PMU_VERSION | (implicit) |
| — | G_PMU_ID | Add from SYNC version nibble bits 3–0. |
| FORMAT | FORMAT | No change. |
| PHNMR/ANNMR/DGNMR | same | No change. |
| — | FRNMR | Set to 1. |
| — | DFDTNMR | Set to 1. |
| CHNAM | CHNAM | No change. |
| PHSCALE 12×PHNMR | PHSCALE 16×PHNMR | Add 4th word (voltage class) = 0.0. |
| — | FRSCALE | Set 1.0 / 0.0. |
| — | DFDTSCALE | Set 1.0 / 0.0. |
| ANSCALE | ANSCALE | No change. |
| DIGUNIT | DIGUNIT | No change. |
| PMU_LAT/LON/ELEV | same | No change. |
| SVC_CLASS (1 B) | PMUFLAG (2 B) | 15–14→0; bit13←FNOM; bit12→0; bit11 set if data retrievable; 10–4→0; 3–0←SVC_CLASS. |
| WINDOW/GRP_DLY | same | No change. |
| — | PMU_DATA_RATE | Set = incoming DATA_RATE. |
| FNOM (2 B) | — | Delete (folded into PMUFLAG bit 13). |
| CFGCNT | CFGCNT | No change. |
| DATA_RATE | STREAM_DATA_RATE | No change (renamed). |
| — | WAIT_TIME | Add. |
| CHK | CHK | Recalc. |

**From CFG-2 (Table H.7)** — lossy; synthesize missing metadata:
| v2 field | v3 field | Action |
|---|---|---|
| — | CONT_IDX | Recalc. |
| TIME_BASE | TIME_BASE | No change. |
| — | PDC_NAME | Add. |
| NUM_PMU | NUM_PMU | No change. |
| STN | PMU_NAME | No change (renamed). |
| IDCODE | PMU_ID | No change (renamed). |
| — | PMU_VERSION | Add from SYNC bits 3–0. |
| — | G_PMU_ID | Set to 0. |
| FORMAT | FORMAT | No change. |
| PHNMR/ANNMR/DGNMR | same | No change. |
| — | FRNMR / DFDTNMR | Set to 1 each. |
| CHNAM | CHNAM | No change. |
| PHUNIT | PHSCALE 16×PHNMR | Word1: bytes0–1=0; byte3 bit3=voltage/current from PHUNIT MSB, bits2–0=`111`; byte4 user. Word2=1.0 if float, else compute from PHUNIT low 3 bytes. Word3=0.0. Word4(voltage class)=0.0. |
| — | FRSCALE / DFDTSCALE | Set 1.0 / 0.0. |
| ANUNIT | ANSCALE | Add; 1.0/0.0 if unavailable. |
| DIGUNIT | DIGUNIT | No change. |
| — | PMU_LAT/LON/ELEV | Set NaN. |
| — | PMUFLAG (2 B) | 15–14→0; bit13←FNOM; bit12→0; bit11 set if retrievable; 10–4→0; 3–0=`111`. |
| — | WINDOW / GRP_DLY | Set −1 each. |
| — | PMU_DATA_RATE | Set = incoming DATA_RATE. |
| FNOM (2 B) | — | Delete. |
| CFGCNT | CFGCNT | No change. |
| DATA_RATE | STREAM_DATA_RATE | No change (renamed). |
| — | WAIT_TIME | Add. |
| CHK | CHK | Recalc. |

### 10.7 Data conversion v2 → v3 (Table H.8)
| v2 field | v3 field | Action |
|---|---|---|
| STAT | STAT_FLAG | See §10.8. |
| — | TIMEQUALITY | Build per Table 7 (v2→v3 conversion, §4.2). |
| PHASORS | PHASOR (+DAPHASOR) | Exclude data attributes (PMUFLAG bit12=0). |
| FREQ | FREQ (+DAFREQ) | Exclude data attributes. |
| DFREQ | DFREQ (+DADFREQ) | Exclude data attributes. |
| ANALOG | ANALOG | No change. |
| DIGITAL | DIGITAL | No change. |

### 10.8 STAT (v2) → STAT_FLAG (v3) (Table H.9)
| v3 STAT_FLAG bits | Meaning | Action from v2 STAT |
|---|---|---|
| 15–14 | Data error | Copy from v2 bits 15–14. |
| 13 | PMU sync | 0 if v2 PMU_TQ (bits 8–6) is 0 or 1; else 1. |
| 12 | Data sorting | Copy from v2 bit 12. |
| 11 | Data bad | Set normally for v3. |
| 10 | Inserted data | Set normally for v3. |
| 9–0 | Reserved | Set 0. |

---

## 11. v2 fixture generation checklist (no capture available)

To produce a byte-correct v2 test fixture from scratch:
- [ ] SYNC = `0xAA` + **v2 second byte per §9.1** (nibble 0010 → 0x02/0x12/0x22/0x32/0x42; 0x52 CFG-3). Do **not** use the Annex A v1 hex (0x01/21/31/41).
- [ ] Big-endian everywhere; **FRACSEC is 4 B** (TQ byte + 24-bit count), not 3 B.
- [ ] CFG-1/CFG-2: fixed 16 B ASCII for STN and every CHNAM; PHUNIT 4 B/phasor, ANUNIT 4 B/analog, DIGUNIT 4 B/digital.
- [ ] Exactly one FREQ + one DFREQ per PMU (no FRNMR/DFDTNMR fields).
- [ ] FNOM bit0 = 50/60 Hz; DATA_RATE encoding per §9.4.
- [ ] Data frame: STAT (v2 layout §9.3.1), then phasors/freq/dfreq/analog/digital, repeat per PMU, CHK last.
- [ ] CRC-CCITT (poly x^16+x^12+x^5+1, seed 0xFFFF, no final mask) over SYNC…last payload byte.
- [ ] For round-trip tests, run the fixture through §10 conversion tables and assert field-by-field.

---

## 12. Parser/generator checklist

- [ ] SYNC = `0xAA` + correct type/version nibble (v3 = 0011; v2 = 0010 — see §9.1).
- [ ] Big-endian for all words.
- [ ] FRAMESIZE matches actual byte count incl. CHK.
- [ ] CRC-CCITT verified over SYNC…last-payload byte (poly x^16+x^12+x^5+1, seed 0xFFFF, no final mask).
- [ ] Timestamp reconstructed from SOC + FRACSEC/TIME_BASE (mind v2's 4-B FRACSEC and v3's 1-B LEAP_BYTE + 3-B FRACSEC).
- [ ] Config parsed first; counts (PHNMR/ANNMR/FRNMR/DFDTNMR/DGNMR) and FORMAT drive data-frame parsing.
- [ ] Int vs float value sizes selected per FORMAT bits.
- [ ] Optional data-attributes words handled all-or-nothing per PMUFLAG.
- [ ] Fragmented config reassembled by CONT_IDX.
- [ ] Multi-PMU repetition honored (NUM_PMU).
- [ ] Discrete-event frames handled out of the periodic cadence.
- [ ] Command STREAM_ID validated before execution.
- [ ] Error-response emitted for invalid/unsupported commands (unless configured silent).
- [ ] Version detected from SYNC nibble; convert per §10 if mixing v2/v3.

---

*Extracted for WAMA data-concepts work from the licensed IEEE Std C37.118.2™-2024 PDF held on Elia Group SharePoint. This is a working implementation summary, not a substitute for the normative standard — verify any ambiguous detail against the PDF.*
