//! MO:DCA structured-field parser.
//!
//! An AFP file is a sequence of structured fields. Each one looks like:
//!
//! ```text
//! 0x5A                 carriage-control byte (always 0x5A)
//! length      (2 bytes, big-endian; counts everything after the 0x5A)
//! sf_id       (3 bytes: 0xD3, type code, category code)
//! flags       (1 byte)
//! sequence    (2 bytes)
//! data        (length - 8 bytes; may begin with an 8-byte EBCDIC name
//!              for Begin/End fields, and may carry triplets)
//! ```
//!
//! Reference: MO:DCA Reference, AFPC-0004-10.

use std::fmt;

pub const CARRIAGE_CONTROL: u8 = 0x5A;

/// Structured field identifiers (3-byte IDs), named after their MO:DCA
/// abbreviation; `1`/`2` suffixes tell the formats of PTD, PGP and MCF apart.
pub mod sf {
    /// Begin Document.
    pub const BDT: u32 = 0xD3A8A8;
    /// End Document.
    pub const EDT: u32 = 0xD3A9A8;
    /// Begin Named Page Group.
    pub const BNG: u32 = 0xD3A8AD;
    /// End Named Page Group.
    pub const ENG: u32 = 0xD3A9AD;
    /// Begin Page.
    pub const BPG: u32 = 0xD3A8AF;
    /// End Page.
    pub const EPG: u32 = 0xD3A9AF;
    /// Begin Active Environment Group.
    pub const BAG: u32 = 0xD3A8C9;
    /// End Active Environment Group.
    pub const EAG: u32 = 0xD3A9C9;
    /// Begin Presentation Text.
    pub const BPT: u32 = 0xD3A89B;
    /// End Presentation Text.
    pub const EPT: u32 = 0xD3A99B;
    /// Presentation Text Data.
    pub const PTX: u32 = 0xD3EE9B;
    /// Presentation Text Descriptor, format 1.
    pub const PTD1: u32 = 0xD3A69B;
    /// Presentation Text Descriptor, format 2.
    pub const PTD2: u32 = 0xD3B19B;
    /// Composed Text Control.
    pub const CTC: u32 = 0xD3A79B;
    /// Begin Image.
    pub const BIM: u32 = 0xD3A8FB;
    /// End Image.
    pub const EIM: u32 = 0xD3A9FB;
    /// Image Data Descriptor.
    pub const IDD: u32 = 0xD3A6FB;
    /// Image Picture Data.
    pub const IPD: u32 = 0xD3EEFB;
    /// Begin IM Image Object.
    pub const BII: u32 = 0xD3A87B;
    /// End IM Image Object.
    pub const EII: u32 = 0xD3A97B;
    /// IM Image Input Descriptor.
    pub const IID: u32 = 0xD3A67B;
    /// IM Image Output Control.
    pub const IOC: u32 = 0xD3A77B;
    /// IM Image Cell Position.
    pub const ICP: u32 = 0xD3AC7B;
    /// IM Image Raster Data.
    pub const IRD: u32 = 0xD3EE7B;
    /// Begin Graphics.
    pub const BGR: u32 = 0xD3A8BB;
    /// End Graphics.
    pub const EGR: u32 = 0xD3A9BB;
    /// Graphics Data Descriptor.
    pub const GDD: u32 = 0xD3A6BB;
    /// Graphics Data.
    pub const GAD: u32 = 0xD3EEBB;
    /// Begin Bar Code.
    pub const BBC: u32 = 0xD3A8EB;
    /// End Bar Code.
    pub const EBC: u32 = 0xD3A9EB;
    /// Bar Code Data Descriptor.
    pub const BDD: u32 = 0xD3A6EB;
    /// Bar Code Data.
    pub const BDA: u32 = 0xD3EEEB;
    /// Begin Object Container.
    pub const BOC: u32 = 0xD3A892;
    /// End Object Container.
    pub const EOC: u32 = 0xD3A992;
    /// Object Container Data.
    pub const OCD: u32 = 0xD3EE92;
    /// Map Data Resource.
    pub const MDR: u32 = 0xD3ABC3;
    /// Include Object.
    pub const IOB: u32 = 0xD3AFC3;
    /// Begin Resource Group.
    pub const BRG: u32 = 0xD3A8C6;
    /// End Resource Group.
    pub const ERG: u32 = 0xD3A9C6;
    /// Begin Resource.
    pub const BRS: u32 = 0xD3A8CE;
    /// End Resource.
    pub const ERS: u32 = 0xD3A9CE;
    /// Begin Page Segment.
    pub const BPS: u32 = 0xD3A85F;
    /// End Page Segment.
    pub const EPS: u32 = 0xD3A95F;
    /// Include Page Segment.
    pub const IPS: u32 = 0xD3AF5F;
    /// Begin Overlay.
    pub const BMO: u32 = 0xD3A8DF;
    /// End Overlay.
    pub const EMO: u32 = 0xD3A9DF;
    /// Include Page Overlay.
    pub const IPO: u32 = 0xD3AFD8;
    /// Map Page Overlay.
    pub const MPO: u32 = 0xD3ABD8;
    /// Begin Form Map.
    pub const BFM: u32 = 0xD3A8CD;
    /// End Form Map.
    pub const EFM: u32 = 0xD3A9CD;
    /// Begin Medium Map.
    pub const BMM: u32 = 0xD3A8CC;
    /// End Medium Map.
    pub const EMM: u32 = 0xD3A9CC;
    /// Invoke Medium Map.
    pub const IMM: u32 = 0xD3ABCC;
    /// Begin Form Environment Group.
    pub const BFG: u32 = 0xD3A8C5;
    /// End Form Environment Group.
    pub const EFG: u32 = 0xD3A9C5;
    /// Form Environment Group Descriptor.
    pub const FGD: u32 = 0xD3A6C5;
    /// Page Descriptor.
    pub const PGD: u32 = 0xD3A6AF;
    /// Page Position, format 1.
    pub const PGP1: u32 = 0xD3ACAF;
    /// Page Position, format 2.
    pub const PGP2: u32 = 0xD3B1AF;
    /// Medium Descriptor.
    pub const MDD: u32 = 0xD3A688;
    /// Medium Copy Count.
    pub const MCC: u32 = 0xD3A288;
    /// Medium Modification Control.
    pub const MMC: u32 = 0xD3A788;
    /// Begin Document Environment Group.
    pub const BDG: u32 = 0xD3A8C4;
    /// End Document Environment Group.
    pub const EDG: u32 = 0xD3A9C4;
    /// Begin Object Environment Group.
    pub const BOG: u32 = 0xD3A8C7;
    /// End Object Environment Group.
    pub const EOG: u32 = 0xD3A9C7;
    /// Begin Document Index.
    pub const BDI: u32 = 0xD3A8A7;
    /// End Document Index.
    pub const EDI: u32 = 0xD3A9A7;
    /// Object Area Descriptor.
    pub const OBD: u32 = 0xD3A66B;
    /// Object Area Position.
    pub const OBP: u32 = 0xD3AC6B;
    /// Map Bar Code Object.
    pub const MBC: u32 = 0xD3ABEB;
    /// Map IO Image Object.
    pub const MIO: u32 = 0xD3ABFB;
    /// Map Graphics Object.
    pub const MGO: u32 = 0xD3ABBB;
    /// Begin Font.
    pub const BFN: u32 = 0xD3A889;
    /// End Font.
    pub const EFN: u32 = 0xD3A989;
    /// Font Control.
    pub const FNC: u32 = 0xD3A789;
    /// Font Descriptor.
    pub const FND: u32 = 0xD3A689;
    /// Font Orientation.
    pub const FNO: u32 = 0xD3AE89;
    /// Font Position.
    pub const FNP: u32 = 0xD3AC89;
    /// Font Index.
    pub const FNI: u32 = 0xD38C89;
    /// Font Patterns Map.
    pub const FNM: u32 = 0xD3A289;
    /// Font Patterns.
    pub const FNG: u32 = 0xD3EE89;
    /// Font Name Map.
    pub const FNN: u32 = 0xD3AB89;
    /// Map Coded Font, format 2.
    pub const MCF2: u32 = 0xD3AB8A;
    /// Map Coded Font, format 1.
    pub const MCF1: u32 = 0xD3B18A;
    /// Begin Coded Font.
    pub const BCF: u32 = 0xD3A88A;
    /// End Coded Font.
    pub const ECF: u32 = 0xD3A98A;
    /// Coded Font Control.
    pub const CFC: u32 = 0xD3A78A;
    /// Coded Font Index.
    pub const CFI: u32 = 0xD38C8A;
    /// Begin Code Page.
    pub const BCP: u32 = 0xD3A887;
    /// End Code Page.
    pub const ECP: u32 = 0xD3A987;
    /// Code Page Control.
    pub const CPC: u32 = 0xD3A787;
    /// Code Page Descriptor.
    pub const CPD: u32 = 0xD3A687;
    /// Code Page Index.
    pub const CPI: u32 = 0xD38C87;
    /// Map Page.
    pub const MPG: u32 = 0xD3ABAF;
    /// Map Page Segment.
    pub const MPS: u32 = 0xD3B15F;
    /// Tag Logical Element.
    pub const TLE: u32 = 0xD3A090;
    /// No Operation.
    pub const NOP: u32 = 0xD3EEEE;
    /// Begin Print File.
    pub const BPF: u32 = 0xD3A8A5;
    /// End Print File.
    pub const EPF: u32 = 0xD3A9A5;
}

/// Well-known structured fields, keyed by the full 3-byte ID. Names follow
/// the MO:DCA reference abbreviations.
pub const SF_NAMES: &[(u32, &str)] = &[
    (sf::BDT, "BDT (Begin Document)"),
    (sf::EDT, "EDT (End Document)"),
    (sf::BNG, "BNG (Begin Named Page Group)"),
    (sf::ENG, "ENG (End Named Page Group)"),
    (sf::BPG, "BPG (Begin Page)"),
    (sf::EPG, "EPG (End Page)"),
    (sf::BAG, "BAG (Begin Active Environment Group)"),
    (sf::EAG, "EAG (End Active Environment Group)"),
    (sf::BPT, "BPT (Begin Presentation Text)"),
    (sf::EPT, "EPT (End Presentation Text)"),
    (sf::PTX, "PTX (Presentation Text Data)"),
    (sf::PTD1, "PTD (Presentation Text Descriptor, format 1)"),
    (sf::PTD2, "PTD (Presentation Text Descriptor, format 2)"),
    (sf::CTC, "CTC (Composed Text Control)"),
    (sf::BIM, "BIM (Begin Image)"),
    (sf::EIM, "EIM (End Image)"),
    (sf::IDD, "IDD (Image Data Descriptor)"),
    (sf::IPD, "IPD (Image Picture Data)"),
    (sf::BII, "BII (Begin IM Image Object)"),
    (sf::EII, "EII (End IM Image Object)"),
    (sf::IID, "IID (IM Image Input Descriptor)"),
    (sf::IOC, "IOC (IM Image Output Control)"),
    (sf::ICP, "ICP (IM Image Cell Position)"),
    (sf::IRD, "IRD (IM Image Raster Data)"),
    (sf::BGR, "BGR (Begin Graphics)"),
    (sf::EGR, "EGR (End Graphics)"),
    (sf::GDD, "GDD (Graphics Data Descriptor)"),
    (sf::GAD, "GAD (Graphics Data)"),
    (sf::BBC, "BBC (Begin Bar Code)"),
    (sf::EBC, "EBC (End Bar Code)"),
    (sf::BDD, "BDD (Bar Code Data Descriptor)"),
    (sf::BDA, "BDA (Bar Code Data)"),
    (sf::BOC, "BOC (Begin Object Container)"),
    (sf::EOC, "EOC (End Object Container)"),
    (sf::OCD, "OCD (Object Container Data)"),
    (sf::MDR, "MDR (Map Data Resource)"),
    (sf::IOB, "IOB (Include Object)"),
    (sf::BRG, "BRG (Begin Resource Group)"),
    (sf::ERG, "ERG (End Resource Group)"),
    (sf::BRS, "BRS (Begin Resource)"),
    (sf::ERS, "ERS (End Resource)"),
    (sf::BPS, "BPS (Begin Page Segment)"),
    (sf::EPS, "EPS (End Page Segment)"),
    (sf::IPS, "IPS (Include Page Segment)"),
    (sf::BMO, "BMO (Begin Overlay)"),
    (sf::EMO, "EMO (End Overlay)"),
    (sf::IPO, "IPO (Include Page Overlay)"),
    (sf::MPO, "MPO (Map Page Overlay)"),
    (sf::BFM, "BFM (Begin Form Map)"),
    (sf::EFM, "EFM (End Form Map)"),
    (sf::BMM, "BMM (Begin Medium Map)"),
    (sf::EMM, "EMM (End Medium Map)"),
    (sf::IMM, "IMM (Invoke Medium Map)"),
    (sf::BFG, "BFG (Begin Form Environment Group)"),
    (sf::EFG, "EFG (End Form Environment Group)"),
    (sf::FGD, "FGD (Form Environment Group Descriptor)"),
    (sf::PGD, "PGD (Page Descriptor)"),
    (sf::PGP1, "PGP (Page Position, format 1)"),
    (sf::PGP2, "PGP (Page Position, format 2)"),
    (sf::MDD, "MDD (Medium Descriptor)"),
    (sf::MCC, "MCC (Medium Copy Count)"),
    (sf::MMC, "MMC (Medium Modification Control)"),
    (sf::BDG, "BDG (Begin Document Environment Group)"),
    (sf::EDG, "EDG (End Document Environment Group)"),
    (sf::BOG, "BOG (Begin Object Environment Group)"),
    (sf::EOG, "EOG (End Object Environment Group)"),
    (sf::BDI, "BDI (Begin Document Index)"),
    (sf::EDI, "EDI (End Document Index)"),
    (sf::OBD, "OBD (Object Area Descriptor)"),
    (sf::OBP, "OBP (Object Area Position)"),
    (sf::MBC, "MBC (Map Bar Code Object)"),
    (sf::MIO, "MIO (Map IO Image Object)"),
    (sf::MGO, "MGO (Map Graphics Object)"),
    (sf::BFN, "BFN (Begin Font)"),
    (sf::EFN, "EFN (End Font)"),
    (sf::FNC, "FNC (Font Control)"),
    (sf::FND, "FND (Font Descriptor)"),
    (sf::FNO, "FNO (Font Orientation)"),
    (sf::FNP, "FNP (Font Position)"),
    (sf::FNI, "FNI (Font Index)"),
    (sf::FNM, "FNM (Font Patterns Map)"),
    (sf::FNG, "FNG (Font Patterns)"),
    (sf::FNN, "FNN (Font Name Map)"),
    (sf::MCF2, "MCF (Map Coded Font, format 2)"),
    (sf::MCF1, "MCF (Map Coded Font, format 1)"),
    (sf::BCF, "BCF (Begin Coded Font)"),
    (sf::ECF, "ECF (End Coded Font)"),
    (sf::CFC, "CFC (Coded Font Control)"),
    (sf::CFI, "CFI (Coded Font Index)"),
    (sf::BCP, "BCP (Begin Code Page)"),
    (sf::ECP, "ECP (End Code Page)"),
    (sf::CPC, "CPC (Code Page Control)"),
    (sf::CPD, "CPD (Code Page Descriptor)"),
    (sf::CPI, "CPI (Code Page Index)"),
    (sf::MPG, "MPG (Map Page)"),
    (sf::MPS, "MPS (Map Page Segment)"),
    (sf::TLE, "TLE (Tag Logical Element)"),
    (sf::NOP, "NOP (No Operation)"),
    (sf::BPF, "BPF (Begin Print File)"),
    (sf::EPF, "EPF (End Print File)"),
];

/// Begin type code.
pub const TYPE_BEGIN: u8 = 0xA8;
/// End type code.
pub const TYPE_END: u8 = 0xA9;

/// Human-readable name for a structured field ID, e.g. `BPG (Begin Page)`.
pub fn sf_name(sf_id: u32) -> String {
    SF_NAMES
        .iter()
        .find(|(id, _)| *id == sf_id)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("Unknown (0x{sf_id:06X})"))
}

/// One MO:DCA structured field, as found in the file.
#[derive(Clone, PartialEq, Eq)]
pub struct StructuredField {
    /// Byte offset of the 0x5A introducer in the file.
    pub offset: usize,
    pub sf_id: u32,
    pub flags: u8,
    pub sequence: u16,
    pub data: Vec<u8>,
}

impl fmt::Debug for StructuredField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SF@{} 0x{:06X} ({} bytes)", self.offset, self.sf_id, self.data.len())
    }
}

impl StructuredField {
    /// The MO:DCA type code (Begin, End, Data, ...).
    pub fn type_code(&self) -> u8 {
        ((self.sf_id >> 8) & 0xFF) as u8
    }

    /// Human-readable abbreviation, e.g. `BPG (Begin Page)`.
    pub fn name(&self) -> String {
        sf_name(self.sf_id)
    }

    /// The 8-byte EBCDIC object name on Begin/End fields, if present.
    pub fn token_name(&self) -> Option<String> {
        let t = self.type_code();
        if (t != TYPE_BEGIN && t != TYPE_END) || self.data.len() < 8 {
            return None;
        }
        Some(crate::codepages::ebcdic_strip(&self.data[..8]))
    }
}

/// Returned when a file does not parse as AFP: what is wrong, and the
/// offset of the structured field where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfpParseError {
    pub offset: usize,
    pub kind: ParseErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// The byte where a field should start is not 0x5A (this byte instead).
    CarriageControl(u8),
    /// The file ends inside a field header.
    Truncated,
    /// The field length is below 8 or runs past the end of the file.
    BadLength(usize),
}

impl fmt::Display for AfpParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pos = self.offset;
        match self.kind {
            ParseErrorKind::CarriageControl(b) => {
                write!(f, "expected 0x5A carriage-control byte at offset {pos}, found 0x{b:02X}")
            }
            ParseErrorKind::Truncated => write!(f, "truncated structured field at offset {pos}"),
            ParseErrorKind::BadLength(length) => write!(f, "structured field at offset {pos} has bad length {length}"),
        }
    }
}

impl std::error::Error for AfpParseError {}

/// Parse every structured field in an AFP byte stream.
///
/// Fails on the first malformed field (bad carriage-control byte or a field
/// that runs past the end of the data), like the original parser: no
/// partial result.
pub fn parse_fields(data: &[u8]) -> Result<Vec<StructuredField>, AfpParseError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        if data[pos] != CARRIAGE_CONTROL {
            return Err(AfpParseError { offset: pos, kind: ParseErrorKind::CarriageControl(data[pos]) });
        }
        if pos + 9 > data.len() {
            return Err(AfpParseError { offset: pos, kind: ParseErrorKind::Truncated });
        }
        let length = u16::from_be_bytes([data[pos + 1], data[pos + 2]]) as usize;
        if length < 8 || pos + 1 + length > data.len() {
            return Err(AfpParseError { offset: pos, kind: ParseErrorKind::BadLength(length) });
        }
        out.push(StructuredField {
            offset: pos,
            sf_id: u32::from_be_bytes([0, data[pos + 3], data[pos + 4], data[pos + 5]]),
            flags: data[pos + 6],
            sequence: u16::from_be_bytes([data[pos + 7], data[pos + 8]]),
            data: data[pos + 9..pos + 1 + length].to_vec(),
        });
        pos += 1 + length;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(sf_id: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x5A];
        out.extend_from_slice(&((data.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(&sf_id.to_be_bytes()[1..]);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn parses_begin_with_token() {
        let mut bytes = sf(sf::BDT, &[0xC4, 0xD6, 0xC3, 0xF0, 0xF0, 0xF0, 0xF0, 0xF1]);
        bytes.extend(sf(sf::EDT, &[]));
        let fields = parse_fields(&bytes).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name(), "BDT (Begin Document)");
        assert_eq!(fields[0].token_name().as_deref(), Some("DOC00001"));
        assert_eq!(fields[1].offset, 17);
        assert_eq!(fields[1].token_name(), None);
    }

    #[test]
    fn errors_match_original_messages() {
        assert_eq!(
            parse_fields(&[0x00]).unwrap_err().to_string(),
            "expected 0x5A carriage-control byte at offset 0, found 0x00"
        );
        assert_eq!(parse_fields(&[0x5A, 0x00]).unwrap_err().to_string(), "truncated structured field at offset 0");
        assert_eq!(
            parse_fields(&[0x5A, 0x00, 0x20, 0xD3, 0xA8, 0xA8, 0, 0, 0]).unwrap_err().to_string(),
            "structured field at offset 0 has bad length 32"
        );
    }

    #[test]
    fn unknown_name() {
        assert_eq!(sf_name(0xD3FFFF), "Unknown (0xD3FFFF)");
    }
}
