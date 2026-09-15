#[repr(u8)]
pub enum DataType {
    Character = 0,
    Data = 2,
    Float = 3,
    Int = 4,
    String = 5,
}


impl DataType {
    pub const BOOLEAN: DataType = DataType::Character;

    pub const AMT: DataType = DataType::Float;
    pub const QTY: DataType = DataType::Float;
    pub const PRICE: DataType = DataType::Float;
    pub const PERCENTAGE: DataType = DataType::Float;
    pub const PRICE_OFFSET: DataType = DataType::Float;

    pub const SEQ_NUM: DataType = DataType::Int;
    pub const TAG_NUM: DataType = DataType::Int;
    pub const LENGTH: DataType = DataType::Int;
    pub const DAY_OF_MONTH: DataType = DataType::Int;
    pub const NUM_IN_GROUP: DataType = DataType::Int;

    pub const COUNTRY: DataType = DataType::String;
    pub const CURRENCY: DataType = DataType::String;
    pub const EXCHANGE: DataType = DataType::String;
    pub const MONTH_YEAR: DataType = DataType::String;
    pub const UTC_DATE_ONLY: DataType = DataType::String;
    pub const UTC_TIME_ONLY: DataType = DataType::String;
    pub const UTC_TIMESTAMP: DataType = DataType::String;
    pub const LOCAL_MKT_DATE: DataType = DataType::String;
    pub const MULTIPLE_VALUE_STRING: DataType = DataType::String;
}