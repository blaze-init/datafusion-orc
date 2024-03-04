use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BinaryBuilder, BooleanBuilder, PrimitiveBuilder, StringArray, StringBuilder,
    StringDictionaryBuilder,
};
use arrow::datatypes::ArrowPrimitiveType;
use arrow::datatypes::{
    Date32Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type, SchemaRef,
    TimestampNanosecondType, UInt64Type,
};
use arrow::record_batch::RecordBatch;
use snafu::ResultExt;

use crate::arrow_reader::column::binary::new_binary_iterator;
use crate::arrow_reader::column::boolean::new_boolean_iter;
use crate::arrow_reader::column::float::new_float_iter;
use crate::arrow_reader::column::int::new_int_iter;
use crate::arrow_reader::column::string::StringDecoder;
use crate::arrow_reader::column::timestamp::new_timestamp_iter;
use crate::arrow_reader::column::NullableIterator;
use crate::error::{self, Result};
use crate::schema::DataType;
use crate::stripe::Stripe;

use self::list::ListArrayDecoder;
use self::map::MapArrayDecoder;
use self::struct_decoder::StructArrayDecoder;

use super::column::tinyint::new_i8_iter;
use super::column::Column;

pub mod list;
pub mod map;
pub mod struct_decoder;

struct PrimitiveArrayDecoder<T: ArrowPrimitiveType> {
    inner: NullableIterator<T::Native>,
}

impl<T: ArrowPrimitiveType> PrimitiveArrayDecoder<T> {
    pub fn new(inner: NullableIterator<T::Native>) -> Self {
        Self { inner }
    }
}

impl<T: ArrowPrimitiveType> ArrayBatchDecoder for PrimitiveArrayDecoder<T> {
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>> {
        let mut builder = PrimitiveBuilder::<T>::with_capacity(batch_size);

        let mut iter = self.inner.by_ref().take(batch_size);
        if let Some(parent_present) = parent_present {
            debug_assert_eq!(
                parent_present.len(),
                batch_size,
                "when provided, parent_present length must equal batch_size"
            );

            for &is_present in parent_present {
                if is_present {
                    // TODO: return as error instead
                    let opt = iter
                        .next()
                        .transpose()?
                        .expect("array less than expected length");
                    builder.append_option(opt);
                } else {
                    builder.append_null();
                }
            }
        } else {
            for opt in iter {
                let opt = opt?;
                builder.append_option(opt);
            }
        };

        let array = Arc::new(builder.finish());
        if array.is_empty() {
            Ok(None)
        } else {
            Ok(Some(array))
        }
    }
}

type Int64ArrayDecoder = PrimitiveArrayDecoder<Int64Type>;
type Int32ArrayDecoder = PrimitiveArrayDecoder<Int32Type>;
type Int16ArrayDecoder = PrimitiveArrayDecoder<Int16Type>;
type Int8ArrayDecoder = PrimitiveArrayDecoder<Int8Type>;
type Float32ArrayDecoder = PrimitiveArrayDecoder<Float32Type>;
type Float64ArrayDecoder = PrimitiveArrayDecoder<Float64Type>;
type TimestampArrayDecoder = PrimitiveArrayDecoder<TimestampNanosecondType>;
type DateArrayDecoder = PrimitiveArrayDecoder<Date32Type>; // TODO: does ORC encode as i64 or i32?

struct BooleanArrayDecoder {
    inner: NullableIterator<bool>,
}

impl BooleanArrayDecoder {
    pub fn new(inner: NullableIterator<bool>) -> Self {
        Self { inner }
    }
}

impl ArrayBatchDecoder for BooleanArrayDecoder {
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>> {
        let mut builder = BooleanBuilder::with_capacity(batch_size);

        let mut iter = self.inner.by_ref().take(batch_size);
        if let Some(parent_present) = parent_present {
            debug_assert_eq!(
                parent_present.len(),
                batch_size,
                "when provided, parent_present length must equal batch_size"
            );

            for &is_present in parent_present {
                if is_present {
                    // TODO: return as error instead
                    let opt = iter
                        .next()
                        .transpose()?
                        .expect("array less than expected length");
                    builder.append_option(opt);
                } else {
                    builder.append_null();
                }
            }
        } else {
            for opt in iter {
                let opt = opt?;
                builder.append_option(opt);
            }
        };

        let array = Arc::new(builder.finish());
        if array.is_empty() {
            Ok(None)
        } else {
            Ok(Some(array))
        }
    }
}

struct DirectStringArrayDecoder {
    inner: NullableIterator<String>,
}

impl DirectStringArrayDecoder {
    pub fn new(inner: NullableIterator<String>) -> Self {
        Self { inner }
    }
}

impl ArrayBatchDecoder for DirectStringArrayDecoder {
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>> {
        let mut builder = StringBuilder::new();

        let mut iter = self.inner.by_ref().take(batch_size);
        if let Some(parent_present) = parent_present {
            debug_assert_eq!(
                parent_present.len(),
                batch_size,
                "when provided, parent_present length must equal batch_size"
            );

            for &is_present in parent_present {
                if is_present {
                    // TODO: return as error instead
                    let opt = iter
                        .next()
                        .transpose()?
                        .expect("array less than expected length");
                    builder.append_option(opt);
                } else {
                    builder.append_null();
                }
            }
        } else {
            for opt in iter {
                let opt = opt?;
                builder.append_option(opt);
            }
        };

        let array = Arc::new(builder.finish());
        if array.is_empty() {
            Ok(None)
        } else {
            Ok(Some(array))
        }
    }
}

struct DictionaryStringArrayDecoder {
    indexes: NullableIterator<u64>,
    dictionary: Arc<StringArray>,
}

impl DictionaryStringArrayDecoder {
    pub fn new(indexes: NullableIterator<u64>, dictionary: Arc<StringArray>) -> Self {
        Self {
            indexes,
            dictionary,
        }
    }
}

impl ArrayBatchDecoder for DictionaryStringArrayDecoder {
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>> {
        // Safety: keys won't overflow
        let mut builder = StringDictionaryBuilder::<UInt64Type>::new_with_dictionary(
            batch_size,
            &self.dictionary,
        )
        .unwrap();

        let mut indexes = self.indexes.by_ref().take(batch_size);
        if let Some(parent_present) = parent_present {
            debug_assert_eq!(
                parent_present.len(),
                batch_size,
                "when provided, parent_present length must equal batch_size"
            );

            for &is_present in parent_present {
                if is_present {
                    // TODO: return as error instead
                    let index = indexes
                        .next()
                        .transpose()?
                        .expect("array less than expected length");
                    builder.append_option(index.map(|idx| self.dictionary.value(idx as usize)));
                } else {
                    builder.append_null();
                }
            }
        } else {
            for index in indexes {
                let index = index?;
                builder.append_option(index.map(|idx| self.dictionary.value(idx as usize)));
            }
        };

        let array = Arc::new(builder.finish());
        if array.is_empty() {
            Ok(None)
        } else {
            Ok(Some(array))
        }
    }
}

struct BinaryArrayDecoder {
    inner: NullableIterator<Vec<u8>>,
}

impl BinaryArrayDecoder {
    pub fn new(inner: NullableIterator<Vec<u8>>) -> Self {
        Self { inner }
    }
}

impl ArrayBatchDecoder for BinaryArrayDecoder {
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>> {
        let mut builder = BinaryBuilder::new();

        let mut iter = self.inner.by_ref().take(batch_size);
        if let Some(parent_present) = parent_present {
            debug_assert_eq!(
                parent_present.len(),
                batch_size,
                "when provided, parent_present length must equal batch_size"
            );

            for &is_present in parent_present {
                if is_present {
                    // TODO: return as error instead
                    let opt = iter
                        .next()
                        .transpose()?
                        .expect("array less than expected length");
                    builder.append_option(opt);
                } else {
                    builder.append_null();
                }
            }
        } else {
            for opt in iter {
                let opt = opt?;
                builder.append_option(opt);
            }
        };

        let array = Arc::new(builder.finish());
        if array.is_empty() {
            Ok(None)
        } else {
            Ok(Some(array))
        }
    }
}

fn merge_parent_present(
    parent_present: &[bool],
    present: impl IntoIterator<Item = bool> + Send,
) -> Vec<bool> {
    // present must have len <= parent_present
    let mut present = present.into_iter();
    let mut merged_present = Vec::with_capacity(parent_present.len());
    for &is_present in parent_present {
        if is_present {
            let p = present.next().expect("array less than expected length");
            merged_present.push(p);
        } else {
            merged_present.push(false);
        }
    }
    merged_present
}

pub struct NaiveStripeDecoder {
    stripe: Stripe,
    schema_ref: SchemaRef,
    decoders: Vec<Box<dyn ArrayBatchDecoder>>,
    index: usize,
    batch_size: usize,
    number_of_rows: usize,
}

impl Iterator for NaiveStripeDecoder {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.number_of_rows {
            let record = self
                .decode_next_batch(self.number_of_rows - self.index)
                .transpose()?;
            self.index += self.batch_size;
            Some(record)
        } else {
            None
        }
    }
}

pub trait ArrayBatchDecoder: Send {
    /// Used as base for decoding ORC columns into Arrow arrays. Provide an input `batch_size`
    /// which specifies the upper limit of the number of values returned in the output array.
    ///
    /// Will return `None` if no more elements to decode (aka return 0 values).
    ///
    /// If parent nested type (e.g. Struct) indicates a null in it's PRESENT stream,
    /// then the child doesn't have a value (similar to other nullability). So we need
    /// to take care to insert these null values as Arrow requires the child to hold
    /// data in the null slot of the child.
    fn next_batch(
        &mut self,
        batch_size: usize,
        parent_present: Option<&[bool]>,
    ) -> Result<Option<ArrayRef>>;
}

pub fn array_decoder_factory(
    column: &Column,
    stripe: &Stripe,
) -> Result<Box<dyn ArrayBatchDecoder>> {
    let decoder: Box<dyn ArrayBatchDecoder> = match column.data_type() {
        DataType::Boolean { .. } => {
            let inner = new_boolean_iter(column, stripe)?;
            Box::new(BooleanArrayDecoder::new(inner))
        }
        DataType::Byte { .. } => {
            let inner = new_i8_iter(column, stripe)?;
            Box::new(Int8ArrayDecoder::new(inner))
        }
        DataType::Short { .. } => {
            let inner = new_int_iter(column, stripe)?;
            Box::new(Int16ArrayDecoder::new(inner))
        }
        DataType::Int { .. } => {
            let inner = new_int_iter(column, stripe)?;
            Box::new(Int32ArrayDecoder::new(inner))
        }
        DataType::Long { .. } => {
            let inner = new_int_iter(column, stripe)?;
            Box::new(Int64ArrayDecoder::new(inner))
        }
        DataType::Float { .. } => {
            let inner = new_float_iter(column, stripe)?;
            Box::new(Float32ArrayDecoder::new(inner))
        }
        DataType::Double { .. } => {
            let inner = new_float_iter(column, stripe)?;
            Box::new(Float64ArrayDecoder::new(inner))
        }
        DataType::String { .. } | DataType::Varchar { .. } | DataType::Char { .. } => {
            let inner = StringDecoder::new(column, stripe)?;
            match inner {
                StringDecoder::Direct(inner) => Box::new(DirectStringArrayDecoder::new(inner)),
                StringDecoder::Dictionary((indexes, dictionary)) => {
                    Box::new(DictionaryStringArrayDecoder::new(indexes, dictionary))
                }
            }
        }
        DataType::Binary { .. } => {
            let inner = new_binary_iterator(column, stripe)?;
            Box::new(BinaryArrayDecoder::new(inner))
        }
        DataType::Decimal { .. } => todo!(),
        DataType::Timestamp { .. } => {
            let inner = new_timestamp_iter(column, stripe)?;
            Box::new(TimestampArrayDecoder::new(inner))
        }
        DataType::TimestampWithLocalTimezone { .. } => todo!(),
        DataType::Date { .. } => {
            let inner = new_int_iter(column, stripe)?;
            Box::new(DateArrayDecoder::new(inner))
        }
        DataType::Struct { .. } => Box::new(StructArrayDecoder::new(column, stripe)?),
        DataType::List { .. } => Box::new(ListArrayDecoder::new(column, stripe)?),
        DataType::Map { .. } => Box::new(MapArrayDecoder::new(column, stripe)?),
        DataType::Union { .. } => todo!(),
    };

    Ok(decoder)
}

impl NaiveStripeDecoder {
    fn inner_decode_next_batch(&mut self, remaining: usize) -> Result<Vec<ArrayRef>> {
        let chunk = self.batch_size.min(remaining);

        let mut fields = Vec::with_capacity(self.stripe.columns.len());

        for decoder in &mut self.decoders {
            match decoder.next_batch(chunk, None)? {
                Some(array) => fields.push(array),
                None => break,
            }
        }

        Ok(fields)
    }

    fn decode_next_batch(&mut self, remaining: usize) -> Result<Option<RecordBatch>> {
        let fields = self.inner_decode_next_batch(remaining)?;

        if fields.is_empty() {
            Ok(None)
        } else {
            //TODO(weny): any better way?
            let fields = self
                .schema_ref
                .fields
                .into_iter()
                .map(|field| field.name())
                .zip(fields)
                .collect::<Vec<_>>();

            Ok(Some(
                RecordBatch::try_from_iter(fields).context(error::ConvertRecordBatchSnafu)?,
            ))
        }
    }

    pub fn new(stripe: Stripe, schema_ref: SchemaRef, batch_size: usize) -> Result<Self> {
        let mut decoders = Vec::with_capacity(stripe.columns.len());
        let number_of_rows = stripe.number_of_rows;

        for col in &stripe.columns {
            let decoder = array_decoder_factory(col, &stripe)?;
            decoders.push(decoder);
        }

        Ok(Self {
            stripe,
            schema_ref,
            decoders,
            index: 0,
            batch_size,
            number_of_rows,
        })
    }
}