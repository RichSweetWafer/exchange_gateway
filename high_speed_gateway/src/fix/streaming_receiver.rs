use tracing::trace;

use super::FixMessage;
use super::flat_index_map::FlatIndexMap;

use super::FixVersion;
use super::decode;

const FIX_SEP: u8 = b'\x01';
const FIX_TAG_VAL_SEP: u8 = b'=';
const FIX_BODY_LENGTH_TAG: u16 = 9;
const FIX_CHECKSUM_TAG: u16 = 10;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RxStage {
    SearchingHeaderStart, // Scanning for "8=FIX"
    ParsingLength,        // Scanning "9=" value
    Reading,              // Streaming exactly `length` bytes
    VerifyingChecksum,    // Scanning trailing checksum
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum FieldRxStage {
    ReadingTag,
    ReadingValue,
}


#[derive(Debug, PartialEq, Clone, Copy)]
struct StageState {
    pub match_idx: usize,          // Matching counter for each RxStage
    pub field_stage: FieldRxStage, // Field parsing stage
    pub field_tag: u16
}

#[derive(Debug,Clone, Copy)]
pub struct StreamingReceiver<const MAX_BUF: usize = 2048> {
    buffer: [u8; MAX_BUF], // TODO: switch to ring buffer impl
    tag_map: FlatIndexMap, 
    cursor: usize,         // Message parsing cursor

    // Parsing stages
    stage: RxStage,          // Message parsing stage
    stage_state: StageState, // State variables for each stage logic

    // Integrity checking
    expected_version_header: [u8; 16],
    expected_version_header_length: usize,
    checksum: u32,          // Message checksum
    expected_length: usize, // Message length
    actual_length: usize,   // Current message length
}

impl StageState {
    pub fn new() -> Self {
        Self {
            match_idx: 0,
            field_stage: FieldRxStage::ReadingTag,
            field_tag: 0,
        }
    }
    pub fn reset(&mut self) {
        self.match_idx = 0;
        self.field_stage = FieldRxStage::ReadingTag;
        self.field_tag = 0;
    }

    pub fn next_state(&mut self) {
        self.match_idx = 0;
        self.field_stage = if self.field_stage == FieldRxStage::ReadingTag {
            trace!(old_state =?self.field_stage, next_state = ?FieldRxStage::ReadingValue);
            FieldRxStage::ReadingValue
        } else {
            trace!(old_state =?self.field_stage, next_state = ?FieldRxStage::ReadingTag);
            FieldRxStage::ReadingTag
        };

    }
}

impl<const MAX_BUF: usize> StreamingReceiver<MAX_BUF> {
    pub fn new(fix_version: FixVersion) -> Self {
        let mut expected_version_header = [0u8; 16];
        let header_prefix = b"8=FIX.";
        expected_version_header[..header_prefix.len()].copy_from_slice(header_prefix);

        let version_bytes = fix_version.as_bytes();
        let version_end = header_prefix.len() + version_bytes.len();
        expected_version_header[header_prefix.len()..version_end].copy_from_slice(version_bytes);
        expected_version_header[version_end] = FIX_SEP;

        Self {
            buffer: [0; MAX_BUF],
            cursor: 0,
            tag_map: FlatIndexMap::new(), 
            stage: RxStage::SearchingHeaderStart,
            stage_state: StageState::new(),
            expected_version_header,
            expected_version_header_length: version_end + 1,
            checksum: 0,
            expected_length: 0,
            actual_length: 0,
        }
    }

    /// Resets the stage of the parser. The underlying buffer is not affected.
    fn reset(&mut self) {
        self.cursor = 0;
        self.stage = RxStage::SearchingHeaderStart;
        self.stage_state.reset();
        self.checksum = 0;
        self.expected_length = 0;
        self.actual_length = 0;
        trace!("reset");
    }

    pub fn clear(&mut self) {
        self.reset();
        self.tag_map.clear();
    }

    /// Feed one byte to the receiver.
    /// Returns: parsing result with optional buffer slice containing the message
    pub fn feed_byte(&mut self, b: u8) -> Result<Option<FixMessage<'_>>, &'static str> {
        if self.cursor >= MAX_BUF {
            self.reset();
            return Err("Streaming buffer bound limit reached");
        }

        self.buffer[self.cursor] = b;
        self.cursor += 1;

        match self.stage {
            RxStage::SearchingHeaderStart => self.searching_header_start(b),
            RxStage::ParsingLength => self.parsing_length(b)?,
            RxStage::Reading => self.read_parse(b)?,
            RxStage::VerifyingChecksum => {
                if self.verifying_checksum(b)? {
                    let end = self.cursor - 1;
                    return Ok(Some(FixMessage {
                                raw: &self.buffer[..end],
                                tag_map: &mut self.tag_map,
                            }))
                }
            }
        }

        Ok(None)
    }

    #[inline(always)]
    fn read_tag(&mut self, b: u8) -> Result<Option<u16>, &'static str> {
        self.stage_state.match_idx += 1;
        if b == FIX_TAG_VAL_SEP {
            let tag_start = self.cursor - self.stage_state.match_idx;
            // exclude '=' from slice
            let tag_slice = &self.buffer[tag_start..(self.cursor - 1)];
            let tag = decode::to_u16(tag_slice).ok_or("Malformed field tag")?;
            return Ok(Some(tag))
        } else if self.stage_state.match_idx == 6 {
            return Err("Field tag out of bounds")
        }

        Ok(None)
    }

    #[inline(always)]
    fn read_value(&mut self, b: u8) -> Result<Option<(usize, usize)>, &'static str> {
        self.stage_state.match_idx += 1;

        if b == FIX_SEP {
            let value_start = self.cursor - self.stage_state.match_idx;
            // let value_slice = &self.buffer[value_start..(self.cursor - 1)];

            // 2. Structural Empty Check (FIX strictly forbids empty "TAG=" values)
            let length = match (self.cursor - 1).checked_sub(value_start) {
                Some(0) | None => return Err("Empty field value encountered"),
                Some(len) => len,
            };

            return Ok(Some((value_start, length)));
        }
        // ASCII bounds check
        else if b < 0x20 || b > 0x7E {
            return Err("Prohibited non-ASCII or control character in field value")
        }

        Ok(None)
    }

    #[inline(always)]
    fn searching_header_start(&mut self, b: u8) {
        if b == self.expected_version_header[self.stage_state.match_idx] {
            self.stage_state.match_idx += 1;
            if self.stage_state.match_idx == self.expected_version_header_length {
                // Match found
                self.checksum = 0;
                // Accumulate checksum
                for i in 0..self.expected_version_header_length {
                    self.checksum += self.buffer[i] as u32;
                }
                // Next stage
                trace!(old_receiver_state = ?self.stage, next_receiver_state = ?RxStage::ParsingLength);
                self.stage = RxStage::ParsingLength;
                self.stage_state.reset();
            }
        } else {
            self.reset();
        }
    }

    #[inline(always)]
    fn parsing_length(&mut self, b: u8) -> Result<(), &'static str> {
        self.checksum += b as u32;

        match self.stage_state.field_stage {
            FieldRxStage::ReadingTag => {
                let tag_option = self.read_tag(b)?;
                if let Some(tag) = tag_option
                {
                    if tag != FIX_BODY_LENGTH_TAG {
                        return Err("BodyLength field was expected after BeginString")
                    }
                    self.stage_state.next_state();
                }
            }
            FieldRxStage::ReadingValue => {
                let value_option = self.read_value(b)?;

                if let Some((offset, length)) = value_option {
                    self.expected_length =
                        decode::to_usize(&self.buffer[offset..(offset + length)]).ok_or("Malformed Body Length field")?;
                    
                    self.stage_state.next_state();
                    self.actual_length = 0;
                    self.stage = RxStage::Reading;
                    trace!(old_receiver_state = ?self.stage, next_receiver_state = ?RxStage::Reading, expected_length = ?self.expected_length);
                }

            }
        }

        Ok(())
    }

    #[inline(always)]
    fn read_parse(&mut self, b: u8) -> Result<(), &'static str> {
        self.checksum += b as u32;
        self.actual_length += 1;

        match self.stage_state.field_stage {
            FieldRxStage::ReadingTag => {
                let tag_option = self.read_tag(b)?;
                if let Some(tag) = tag_option
                {
                    trace!(receiver_state = ?self.stage, parsed_tag = ?tag);
                    self.stage_state.field_tag = tag;
                    self.stage_state.next_state();
                }
            }
            FieldRxStage::ReadingValue => {
                let value_option = self.read_value(b)?;
                if let Some((offset, length)) = value_option {
                    
                    // TODO: FlatIndexMap is for unique fields only, add storage for repeating fields
                    self.tag_map.insert(self.stage_state.field_tag, offset as u32, length as u32)?;
                    trace!(
                        receiver_state = ?self.stage);
                    self.stage_state.next_state();
                    
                }
            }
        }

        // trace!(self.actual_length, self.expected_length);
        if self.actual_length == self.expected_length {
            trace!(old_receiver_state = ?self.stage, next_receiver_state = ?RxStage::VerifyingChecksum);
            self.stage = RxStage::VerifyingChecksum;
            self.stage_state.match_idx = 0;
            self.actual_length = 0;
        }

        Ok(())
    }


    #[inline(always)]
    fn verifying_checksum(&mut self, b: u8) -> Result<bool, &'static str> {
        match self.stage_state.field_stage {
            FieldRxStage::ReadingTag => {
                let tag_option = self.read_tag(b)?;
                if let Some(tag) = tag_option
                {
                    if tag != FIX_CHECKSUM_TAG {
                        return Err("Checksum field was expected at the end")
                    }
                    self.stage_state.next_state();
                }
            }
            FieldRxStage::ReadingValue => {
                let value = self.read_value(b)?;

                if let Some((offset, length)) = value {
                    let expected_checksum = decode::to_u32(&self.buffer[offset..(offset + length)]).ok_or("Malformed Checksum field")?;

                    if self.checksum % 256 != expected_checksum {
                        return Err("Checksum value does not match the message")
                    }
                    return Ok(true)
                }
            }
        }
        
        Ok(false)
    }


}
