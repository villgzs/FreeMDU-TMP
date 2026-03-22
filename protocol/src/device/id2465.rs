//! Miele washing machines with software ID **2465** (e.g. W 667).
//!
//! Identity fields from the type plate are stored as a fixed-layout ASCII record in **RAM**
//! (not the EEPROM block used by [`super::id1998`]). Layout verified against
//! `memory_dump*.bin` from the `dump_memory` example (file offset equals memory address when
//! `START = 0`):
//! - Primary copy: address **`0x19b6`**
//! - Mirror copy: **`0x2e13`** (may omit the trailing 8-digit suffix; reads use the primary)
//!
//! ## Program selection (RAM, from labeled dumps)
//!
//! Correlated against `memory_dump_*deg_cotton*.bin` (same layout as `dump_memory` with `START = 0`):
//! - **Final spin** is stored as **rpm÷10** (one byte). Primary address **`0x2e69`**; the same value
//!   is mirrored at `0x2b70`, `0x2c4a`, `0x32c4`, and `0x386b`.
//! - **Program options** as **little-endian `u16`** at **`0x1d6f`**. In captures, non-eco cotton at
//!   1200 rpm used `0x0108`, eco cotton at 1200 rpm used `0x0105` (bit layout not fully mapped).
//! - Byte **`0x1d60`** is non-zero when that program-memory block is populated; some captures had the
//!   whole `0x1d60`.. region cleared to zero (different UI/power state while dumping).
//! - **Target wash temperature °C** did not line up with a single stable byte across these eight
//!   files; needs more controlled dumps or live reads while changing only temperature.
//!
//! ## How this compares to other washing machine modules in this crate
//!
//! ### Diagnostic unlock
//!
//! | Module | `unlock_read_access` | `unlock_full_access` | Extra `write_memory` (ROM / map) |
//! |--------|---------------------|----------------------|-----------------------------------|
//! | **id2465**, **id1998** | `0x2b67` | `0x8235` | *(none)* |
//! | id324, id360, id469, id629 | `0x43ea` | `0x1f02` | id360 `0x0289`, id469 `0x02e9`, id629 `0x02c2` |
//! | id419 | `0xb4ee` | `0x4e83` | *(none)* |
//!
//! So **2465 matches the id1998 key pair**, not the 43ea/1f02 family used on most other washers here.
//!
//! ### Where program data lives (address “families”)
//!
//! **id324 / id419** use **low** RAM (e.g. program `~0x0071`, options `~0x0012`/`0x0016`, spin `~0x0011`/`0x0015`).
//!
//! **id360 / id469 / id629** share a **mid-RAM** layout (e.g. program `~0x00b5`, options `~0x0058`,
//! spin as [`SpinSetting`](super::id629::SpinSetting) `~0x0057`, actual spin byte `~0x00e9`).
//!
//! **id1998** uses **high** RAM around **`0x1d6x`**: program `0x1d6c`, **target temp °C `0x1d6d`**,
//! **final spin as rpm÷10 `0x1d6e`**, **options `u16` `0x1d6f`**, phase `0x1d76`, timers `0x1d78`–`0x1d7b`,
//! operating time `0x1cd2`, etc.
//!
//! ### Overlap between id2465 and id1998
//!
//! - **`0x1d6f`**: **Same address** as id1998’s program-options word (`read_memory`/`write_memory` as
//!   `u16` LE). id1998 XORs **`0x0040`** when mapping the intensive/short bit. A **hypothesis** decoder
//!   (`ProgramOption::from_bits(raw ^ 0x0040)`) is available for experiments: enable Cargo feature
//!   **`experimental-decode-program-options`**, run `experimental_program_options_ws`, or see
//!   `decode_program_options_id1998_hypothesis` (always available; **not** wired into default
//!   [`Device::properties`]; optional property behind feature **`experimental-decode-program-options`**).
//! - **`0x1d6d` / `0x1d6e`**: On id1998 these are **°C** and **rpm÷10**; on 2465 labeled dumps they
//! stayed **0** while **rpm÷10 remained valid at `0x2e69`** (and mirrors). Treat id1998’s high-RAM
//! program block as **only partially applicable**: options word aligns, spin/temp bytes do not.
//! - **Type plate**: id629/id360 use **EEPROM** near **`0x01ba`**; id1998 uses **EEPROM** near
//!   **`0x02e5`**; **id2465** stores the ASCII block in **RAM `0x19b6`** (EEPROM held no plaintext
//! plate in the reference dump).

use crate::device::{
    Action, Device, DeviceKind, Error, Interface, Property, PropertyKind, Result, Value, private,
};
use alloc::{
    boxed::Box,
    string::{String, ToString},
};
use core::str;
use embedded_io_async::{Read, Write};

macro_rules! compatible_software_ids {
    () => {
        2465
    };
}
pub(super) use compatible_software_ids;

/// Primary type-plate string in RAM (`dump_memory` with `START = 0`).
const TYPE_PLATE_ADDR: u32 = 0x19b6;

/// `E000` + 9-digit serial + 2-digit index + 15-byte model field + 8-digit M-Nr + 8-digit suffix.
const TYPE_PLATE_MIN: usize = 46;

/// Final spin as rpm÷10 (see module docs for mirror addresses).
///
/// **Note:** [`super::id1998`] uses **`0x1d6e`** for the same physical quantity (rpm÷10); on 2465
/// that byte was zero in dumps while this address stayed correct.
const PROGRAM_SPIN_RPM10_ADDR: u32 = 0x2e69;

/// Little-endian program-options word — **same `0x1d6f` as** [`super::id1998`] (bit meanings TBD).
const PROGRAM_OPTIONS_ADDR: u32 = 0x1d6f;

/// Non-zero when the program-parameter block around `0x1d6x` is populated in dumps.
///
/// id1998 does not use this probe; it assumes the `0x1d6x` block is valid whenever the machine is
/// in a state that fills it.
const PROGRAM_CONTEXT_PROBE_ADDR: u32 = 0x1d60;

/// Interprets the raw `u16` at **`0x1d6f`** using [`super::id1998::ProgramOption`] exactly like
/// [`super::id1998::WashingMachine::query_program_options`]: `from_bits(raw ^ 0x0040)`.
///
/// **Hypothesis only.** Returns [`None`] when unknown bits are set after XOR (e.g. eco often uses
/// **`0x0105`** → `0x0145`, which is not a strict `ProgramOption` bit pattern). Use
/// [`decode_program_options_id1998_hypothesis_truncated`] for a best-effort label in that case.
pub fn decode_program_options_id1998_hypothesis(raw: u16) -> Option<super::id1998::ProgramOption> {
    super::id1998::ProgramOption::from_bits(raw ^ 0x0040)
}

/// Same XOR as [`decode_program_options_id1998_hypothesis`], but **drops unknown bits** via
/// [`ProgramOption::from_bits_truncate`](super::id1998::ProgramOption::from_bits_truncate).
///
/// Eco on W667 often reads **`0x0105`**; strict decode fails, but truncate yields e.g.
/// `Soak | IntensiveShort | Starch` — treat as **best-effort**, not verified against every UI state.
pub fn decode_program_options_id1998_hypothesis_truncated(raw: u16) -> super::id1998::ProgramOption {
    super::id1998::ProgramOption::from_bits_truncate(raw ^ 0x0040)
}

const PROP_EQUIPMENT_CODE: Property = Property {
    kind: PropertyKind::General,
    id: "equipment_code",
    name: "Equipment Code",
    unit: None,
};
const PROP_SERIAL_NUMBER: Property = Property {
    kind: PropertyKind::General,
    id: "serial_number",
    name: "Serial Number",
    unit: None,
};
const PROP_SERIAL_NUMBER_INDEX: Property = Property {
    kind: PropertyKind::General,
    id: "serial_number_index",
    name: "Serial Number Index",
    unit: None,
};
const PROP_MODEL_NUMBER: Property = Property {
    kind: PropertyKind::General,
    id: "model_number",
    name: "Model Number",
    unit: None,
};
const PROP_MATERIAL_NUMBER: Property = Property {
    kind: PropertyKind::General,
    id: "material_number",
    name: "Material Number",
    unit: None,
};
const PROP_TYPE_PLATE_EXTRA: Property = Property {
    kind: PropertyKind::General,
    id: "type_plate_extra",
    name: "Type Plate Extra",
    unit: None,
};
const PROP_PROGRAM_SPIN_SPEED: Property = Property {
    kind: PropertyKind::Operation,
    id: "program_spin_speed",
    name: "Program Spin Speed",
    unit: Some("rpm"),
};
const PROP_PROGRAM_OPTIONS_RAW: Property = Property {
    kind: PropertyKind::Operation,
    id: "program_options_raw",
    name: "Program Options (raw)",
    unit: None,
};
const PROP_PROGRAM_MEMORY_VALID: Property = Property {
    kind: PropertyKind::Operation,
    id: "program_memory_valid",
    name: "Program Memory Valid",
    unit: None,
};
#[cfg(feature = "experimental-decode-program-options")]
const PROP_PROGRAM_OPTIONS_ID1998_HYPOTHESIS: Property = Property {
    kind: PropertyKind::Operation,
    id: "program_options_id1998_hypothesis",
    name: "Program Options (id1998 hypothesis)",
    unit: None,
};

#[derive(Debug)]
struct TypePlate {
    equipment_code: String,
    serial_number: String,
    serial_index: String,
    model_number: String,
    material_number: String,
    extra_digits: String,
}

fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_type_plate(raw: &[u8]) -> Option<TypePlate> {
    if raw.len() < TYPE_PLATE_MIN {
        return None;
    }
    if raw.get(..4) != Some(b"E000") {
        return None;
    }

    let equipment_code = str::from_utf8(&raw[0..4]).ok()?.to_string();
    let serial_number = str::from_utf8(&raw[4..13]).ok()?.to_string();
    let serial_index = str::from_utf8(&raw[13..15]).ok()?.to_string();
    let model_raw = str::from_utf8(&raw[15..30]).ok()?;
    let model_number = model_raw.trim_end().to_string();
    let material_number = str::from_utf8(&raw[30..38]).ok()?.to_string();

    let extra_slice = str::from_utf8(&raw[38..TYPE_PLATE_MIN]).ok()?;
    let extra_digits = extra_slice
        .trim_end_matches(|c| c == '\0' || c == ' ')
        .to_string();

    if !is_ascii_digits(&serial_number) || serial_number.len() != 9 {
        return None;
    }
    if !is_ascii_digits(&serial_index) || serial_index.len() != 2 {
        return None;
    }
    if !is_ascii_digits(&material_number) || material_number.len() != 8 {
        return None;
    }
    if !extra_digits.is_empty() && (!is_ascii_digits(&extra_digits) || extra_digits.len() != 8) {
        return None;
    }

    Some(TypePlate {
        equipment_code,
        serial_number,
        serial_index,
        model_number,
        material_number,
        extra_digits,
    })
}

/// Washing machine (software ID 2465).
#[derive(Debug)]
pub struct WashingMachine<P> {
    intf: Interface<P>,
    software_id: u16,
}

impl<P: Read + Write> WashingMachine<P> {
    pub(crate) async fn initialize(
        mut intf: Interface<P>,
        software_id: u16,
    ) -> Result<Self, P::Error> {
        intf.unlock_read_access(0x2b67).await?;
        intf.unlock_full_access(0x8235).await?;

        Ok(Self { intf, software_id })
    }

    async fn read_type_plate(&mut self) -> Result<TypePlate, P::Error> {
        let raw: [u8; TYPE_PLATE_MIN] = self.intf.read_memory(TYPE_PLATE_ADDR).await?;
        parse_type_plate(&raw).ok_or(Error::UnexpectedMemoryValue)
    }

    /// Four-character prefix on the RAM type-plate record (e.g. `E000`).
    pub async fn query_equipment_code(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.equipment_code)
    }

    /// Nine-digit serial number from the type plate.
    pub async fn query_serial_number(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.serial_number)
    }

    /// Two-digit serial index from the type plate.
    pub async fn query_serial_number_index(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.serial_index)
    }

    /// Model designation (space-trimmed), e.g. `W667`.
    pub async fn query_model_number(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.model_number)
    }

    /// Eight-digit material number (M-Nr).
    pub async fn query_material_number(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.material_number)
    }

    /// Eight ASCII digits after the material number when present; meaning not yet decoded.
    pub async fn query_type_plate_extra(&mut self) -> Result<String, P::Error> {
        Ok(self.read_type_plate().await?.extra_digits)
    }

    /// Selected final spin speed in rpm (`byte at 0x2e69` × 10).
    pub async fn query_program_spin_speed(&mut self) -> Result<u16, P::Error> {
        let rpm10: u8 = self.intf.read_memory(PROGRAM_SPIN_RPM10_ADDR).await?;
        Ok(u16::from(rpm10).saturating_mul(10))
    }

    /// Raw little-endian program-options word at `0x1d6f` (see module docs).
    pub async fn query_program_options_raw(&mut self) -> Result<u16, P::Error> {
        Ok(self.intf.read_memory(PROGRAM_OPTIONS_ADDR).await?)
    }

    /// `true` when byte `0x1d60` is non-zero (program block looked valid in reference dumps).
    pub async fn query_program_memory_valid(&mut self) -> Result<bool, P::Error> {
        let b: u8 = self.intf.read_memory(PROGRAM_CONTEXT_PROBE_ADDR).await?;
        Ok(b != 0)
    }

    /// Strict id1998 decode, else **truncated** decode with a short note (requires feature
    /// **`experimental-decode-program-options`**).
    #[cfg(feature = "experimental-decode-program-options")]
    pub async fn query_program_options_id1998_hypothesis_display(
        &mut self,
    ) -> Result<String, P::Error> {
        let raw = self.query_program_options_raw().await?;
        Ok(display_program_options_hypothesis(raw))
    }
}

/// Shared formatting for strict + truncated id1998 `ProgramOption` hypothesis (raw word at `0x1d6f`).
#[cfg(feature = "experimental-decode-program-options")]
fn display_program_options_hypothesis(raw: u16) -> String {
    let xor40 = raw ^ 0x0040;
    match decode_program_options_id1998_hypothesis(raw) {
        Some(opts) => opts.to_string(),
        None => {
            let t = decode_program_options_id1998_hypothesis_truncated(raw);
            if t.is_empty() {
                alloc::format!("(no parse) raw=0x{raw:04x} xor40=0x{xor40:04x}")
            } else {
                alloc::format!("{t} [truncated; xor40=0x{xor40:04x}]")
            }
        }
    }
}

#[async_trait::async_trait(?Send)]
impl<P: Read + Write> Device<P> for WashingMachine<P> {
    async fn connect(port: P) -> Result<Self, P::Error> {
        let mut intf = Interface::new(port);
        let id = intf.query_software_id().await?;

        match id {
            compatible_software_ids!() => Self::initialize(intf, id).await,
            _ => Err(Error::UnknownSoftwareId(id)),
        }
    }

    fn interface(&mut self) -> &mut Interface<P> {
        &mut self.intf
    }

    fn software_id(&self) -> u16 {
        self.software_id
    }

    fn kind(&self) -> DeviceKind {
        DeviceKind::WashingMachine
    }

    fn properties(&self) -> &'static [Property] {
        &[
            PROP_EQUIPMENT_CODE,
            PROP_SERIAL_NUMBER,
            PROP_SERIAL_NUMBER_INDEX,
            PROP_MODEL_NUMBER,
            PROP_MATERIAL_NUMBER,
            PROP_TYPE_PLATE_EXTRA,
            PROP_PROGRAM_SPIN_SPEED,
            PROP_PROGRAM_OPTIONS_RAW,
            PROP_PROGRAM_MEMORY_VALID,
        ]
    }

    fn actions(&self) -> &'static [Action] {
        &[]
    }

    async fn query_property(&mut self, prop: &Property) -> Result<Value, P::Error> {
        match *prop {
            PROP_EQUIPMENT_CODE => Ok(self.query_equipment_code().await?.into()),
            PROP_SERIAL_NUMBER => Ok(self.query_serial_number().await?.into()),
            PROP_SERIAL_NUMBER_INDEX => Ok(self.query_serial_number_index().await?.into()),
            PROP_MODEL_NUMBER => Ok(self.query_model_number().await?.into()),
            PROP_MATERIAL_NUMBER => Ok(self.query_material_number().await?.into()),
            PROP_TYPE_PLATE_EXTRA => Ok(self.query_type_plate_extra().await?.into()),
            PROP_PROGRAM_SPIN_SPEED => Ok(u32::from(self.query_program_spin_speed().await?).into()),
            PROP_PROGRAM_OPTIONS_RAW => Ok(u32::from(self.query_program_options_raw().await?).into()),
            PROP_PROGRAM_MEMORY_VALID => Ok(self.query_program_memory_valid().await?.into()),
            #[cfg(feature = "experimental-decode-program-options")]
            PROP_PROGRAM_OPTIONS_ID1998_HYPOTHESIS => Ok(self
                .query_program_options_id1998_hypothesis_display()
                .await?
                .into()),
            _ => Err(Error::UnknownProperty),
        }
    }

    async fn trigger_action(
        &mut self,
        _action: &Action,
        _param: Option<Value>,
    ) -> Result<(), P::Error> {
        Err(Error::UnknownAction)
    }
}

impl<P> private::Sealed for WashingMachine<P> {}

