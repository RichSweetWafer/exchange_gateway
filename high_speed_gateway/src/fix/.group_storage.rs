


// Max group field references storage size for data spikes (close to ~ 1 Mb messages)
const MAX_GROUP_STORAGE: usize = 128_000;

// Max unique group types defined 
const MAX_GROUP_TYPES: usize = 64;

// Max depth for nested groups in a message
const MAX_NESTED_DEPTH: usize = 4;


#[derive(Debug, Clone, Copy, Default)]
pub struct GroupFieldRef {
    pub tag: u16, // field tag
    pub field_ref: FieldRef, // offset & length
    pub group_id: u16, // group id
    pub entry_idx: u16, // entry index in the group
}

pub struct GroupContext {
    pub group_id: u16, // (tag) Id of the group currently being parsed
    pub delimiter_tag: u16, // Delimiter - the field that splits group into entries (first entry field, usually)  
    pub current_entry_index: i32, // Index of the current entry
}


pub struct GroupStorage {
    storage: Box<[GroupFieldRef; MAX_GROUP_STORAGE]>,
    count: usize,
    parse_stack: [GroupContext; MAX_NESTED_DEPTH],
    stack_depth: i8,
}

impl GroupStorage {
    pub fn new() -> Self {
        // TODO: might be unnecessary 
        let storage = unsafe { Box::new(std::mem::zeroed()) };
        Self {
            storage,
            count: 0,
            parse_stack: [GroupContext {
                group_id: 0,
                delimiter_tag: 0,
                current_entry_index: -1,
            }; MAX_NESTED_DEPTH],
            stack_depth: -1,
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.count = 0;
        self.stack_depth = -1;
    }

    pub fn insert(&mut self, tag: u16, field_ref: FieldRef) {
        self.parse_stack[stack_depth]
        storage[count] = GroupFieldRef {
            tag,
            field_ref,
            group_id,
            entry_idx
        };
    }
}