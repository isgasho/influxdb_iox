use std::cmp::Ordering;
use std::fmt::Debug;

use croaring::Bitmap;
use delorean_arrow::arrow::array::PrimitiveArrayOps;
use delorean_arrow::arrow::array::{Array, PrimitiveArray};
use delorean_arrow::arrow::datatypes::ArrowNumericType;

use crate::column::cmp;

#[derive(Debug)]
pub struct FixedNull<T>
where
    T: ArrowNumericType,
{
    // backing data
    arr: PrimitiveArray<T>,
}

impl<T: ArrowNumericType> std::fmt::Display for FixedNull<T>
where
    T: ArrowNumericType + std::fmt::Debug,
    T::Native: Default
        + PartialEq
        + PartialOrd
        + Copy
        + std::fmt::Debug
        + std::ops::Add<Output = T::Native>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[Arrow<T>] rows: {:?}, nulls: {:?}, size: {}",
            self.arr.len(),
            self.arr.null_count(),
            self.size()
        )
    }
}
impl<T> FixedNull<T>
where
    T: ArrowNumericType,
{
    pub fn num_rows(&self) -> u64 {
        self.arr.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.arr.is_empty()
    }

    /// Returns the total size in bytes of the encoded data. Note, this method
    /// is really an "accurate" estimation. It doesn't include for example the
    /// size of the `Plain` struct receiver.
    pub fn size(&self) -> usize {
        todo!("need to figure this out");
    }

    //
    //
    // ---- Methods for getting row ids from values.
    //
    //

    /// Returns the first logical row that contains a value `v`.
    pub fn first_row_id_eq_value(&self, v: T::Native) -> Option<usize> {
        for i in 0..self.arr.len() {
            if self.arr.is_null(i) {
                continue;
            } else if self.arr.value(i) == v {
                return Some(i);
            }
        }
        None
    }

    //
    //
    // ---- Methods for getting decoded (materialised) values.
    //
    //

    /// Return the logical (decoded) value at the provided row ID. A NULL value
    /// is represented by None.
    pub fn value(&self, row_id: usize) -> Option<T::Native> {
        if self.arr.is_null(row_id) {
            return None;
        }
        Some(self.arr.value(row_id))
    }

    /// Returns the logical (decoded) values for the provided row IDs.
    ///
    /// NULL values are represented by None.
    ///
    /// TODO(edd): Perf - we should return a vector of values and a vector of
    /// integers representing the null validity bitmap.
    pub fn values(
        &self,
        row_ids: &[usize],
        mut dst: Vec<Option<T::Native>>,
    ) -> Vec<Option<T::Native>> {
        dst.clear();
        dst.reserve(row_ids.len());

        for &row_id in row_ids {
            if self.arr.is_null(row_id) {
                dst.push(None)
            } else {
                dst.push(Some(self.arr.value(row_id)))
            }
        }
        assert_eq!(dst.len(), row_ids.len());
        dst
    }

    /// Returns the logical (decoded) values for all the rows in the column.
    ///
    /// NULL values are represented by None.
    ///
    /// TODO(edd): Perf - we should return a vector of values and a vector of
    /// integers representing the null validity bitmap.
    pub fn all_values(&self, mut dst: Vec<Option<T::Native>>) -> Vec<Option<T::Native>> {
        dst.clear();
        dst.reserve(self.arr.len());

        for i in 0..self.num_rows() as usize {
            if self.arr.is_null(i) {
                dst.push(None)
            } else {
                dst.push(Some(self.arr.value(i)))
            }
        }
        assert_eq!(dst.len(), self.num_rows() as usize);
        dst
    }

    //
    //
    // ---- Methods for aggregation.
    //
    //

    /// Returns the count of the non-null values for the provided
    /// row IDs.
    ///
    /// Since this encoding cannot have NULL values this is just the number of
    /// rows requested.
    pub fn count(&self, row_ids: &[usize]) -> u64 {
        if self.arr.null_count() == 0 {
            return row_ids.len() as u64;
        }

        let mut count = 0;
        for &i in row_ids {
            if self.arr.is_null(i) {
                continue;
            }
            count += 1;
        }
        count
    }

    /// Returns the summation of the non-null logical (decoded) values for the
    /// provided row IDs.
    pub fn sum(&self, row_ids: &[usize]) -> Option<T::Native>
    where
        T::Native: std::ops::Add<Output = T::Native>,
    {
        let mut result = T::Native::default();

        if self.arr.null_count() == 0 {
            for chunks in row_ids.chunks_exact(4) {
                result = result + self.arr.value(chunks[3]);
                result = result + self.arr.value(chunks[2]);
                result = result + self.arr.value(chunks[1]);
                result = result + self.arr.value(chunks[0]);
            }

            let rem = row_ids.len() % 4;
            for &i in &row_ids[row_ids.len() - rem..row_ids.len()] {
                result = result + self.arr.value(i);
            }

            return Some(result);
        }

        let mut is_none = true;
        for &i in row_ids {
            if self.arr.is_null(i) {
                continue;
            }
            is_none = false;
            result = result + self.arr.value(i);
        }

        if is_none {
            return None;
        }
        Some(result)
    }

    /// Returns the first logical (decoded) value from the provided
    /// row IDs.
    pub fn first(&self, row_ids: &[usize]) -> Option<T::Native> {
        self.value(row_ids[0])
    }

    /// Returns the last logical (decoded) value from the provided
    /// row IDs.
    pub fn last(&self, row_ids: &[usize]) -> Option<T::Native> {
        self.value(row_ids[row_ids.len() - 1])
    }

    /// Returns the minimum logical (decoded) non-null value from the provided
    /// row IDs.
    pub fn min(&self, row_ids: &[usize]) -> Option<T::Native> {
        let mut min: Option<T::Native> = self.value(row_ids[0]);
        for &v in row_ids.iter().skip(1) {
            if self.arr.is_null(v) {
                continue;
            }

            if self.value(v) < min {
                min = self.value(v);
            }
        }
        min
    }

    /// Returns the maximum logical (decoded) non-null value from the provided
    /// row IDs.
    pub fn max(&self, row_ids: &[usize]) -> Option<T::Native> {
        let mut max: Option<T::Native> = self.value(row_ids[0]);
        for &v in row_ids.iter().skip(1) {
            if self.arr.is_null(v) {
                continue;
            }

            if self.value(v) > max {
                max = self.value(v);
            }
        }
        max
    }

    //
    //
    // ---- Methods for filtering via operators.
    //
    //

    /// Returns the set of row ids that satisfy a binary operator on a logical
    /// value.
    ///
    /// Essentially, this supports `value {=, !=, >, >=, <, <=} x`.
    ///
    /// The equivalent of `IS NULL` is not currently supported via this method.
    pub fn row_ids_filter(&self, value: T::Native, op: cmp::Operator, bm: Bitmap) -> Bitmap {
        match op {
            cmp::Operator::GT => self.row_ids_cmp_order_bm(value, Self::ord_from_op(&op), bm),
            cmp::Operator::GTE => self.row_ids_cmp_order_bm(value, Self::ord_from_op(&op), bm),
            cmp::Operator::LT => self.row_ids_cmp_order_bm(value, Self::ord_from_op(&op), bm),
            cmp::Operator::LTE => self.row_ids_cmp_order_bm(value, Self::ord_from_op(&op), bm),
            _ => self.row_ids_equal_bm(value, op, bm),
        }
    }

    // Helper function to convert comparison operators to cmp orderings.
    fn ord_from_op(op: &cmp::Operator) -> (Ordering, Ordering) {
        match op {
            cmp::Operator::GT => (Ordering::Greater, Ordering::Greater),
            cmp::Operator::GTE => (Ordering::Greater, Ordering::Equal),
            cmp::Operator::LT => (Ordering::Less, Ordering::Less),
            cmp::Operator::LTE => (Ordering::Less, Ordering::Equal),
            _ => panic!("cannot convert operator to ordering"),
        }
    }

    // Handles finding all rows that match the provided operator on `value`.
    // For performance reasons ranges of matching values are collected up and
    // added in bulk to the bitmap.
    fn row_ids_equal_bm(&self, value: T::Native, op: cmp::Operator, mut bm: Bitmap) -> Bitmap {
        bm.clear();

        let desired;
        if let cmp::Operator::Equal = op {
            desired = true; // == operator
        } else {
            desired = false; // != operator
        }

        let mut found = false;
        let mut count = 0;
        for i in 0..self.num_rows() as usize {
            let mut cmp_result: bool;
            let cmp_result = self.arr.value(i) == value;

            if (self.arr.is_null(i) || cmp_result != desired) && found {
                let (min, max) = (i as u64 - count as u64, i as u64);
                bm.add_range(min..max);
                found = false;
                count = 0;
                continue;
            } else if self.arr.is_null(i) || cmp_result != desired {
                continue;
            }

            if !found {
                found = true;
            }
            count += 1;
        }

        // add any remaining range.
        if found {
            let (min, max) = (self.num_rows() - count as u64, self.num_rows());
            bm.add_range(min..max);
        }
        bm
    }

    // Handles finding all rows that match the provided operator on `value`.
    // For performance reasons ranges of matching values are collected up and
    // added in bulk to the bitmap.
    //
    // `op` is a tuple of comparisons where at least one of them must be
    // satisfied to satisfy the overall operator.
    fn row_ids_cmp_order_bm(
        &self,
        value: T::Native,
        op: (std::cmp::Ordering, std::cmp::Ordering),
        mut bm: Bitmap,
    ) -> Bitmap {
        bm.clear();

        let mut found = false;
        let mut count = 0;
        for i in 0..self.num_rows() as usize {
            let cmp_result = self.arr.value(i).partial_cmp(&value);

            if (self.arr.is_null(i) || (cmp_result != Some(op.0) && cmp_result != Some(op.1)))
                && found
            {
                let (min, max) = (i as u64 - count as u64, i as u64);
                bm.add_range(min..max);
                found = false;
                count = 0;
                continue;
            } else if self.arr.is_null(i) || (cmp_result != Some(op.0) && cmp_result != Some(op.1))
            {
                continue;
            }

            if !found {
                found = true;
            }
            count += 1;
        }

        // add any remaining range.
        if found {
            let (min, max) = (self.num_rows() - count as u64, self.num_rows());
            bm.add_range(min..max);
        }
        bm
    }

    /// Returns the set of row ids that satisfy a pair of binary operators
    /// against two values of the same logical type.
    ///
    /// This method is a special case optimisation for common cases where one
    /// wishes to do the equivalent of WHERE x > y AND x <= y` for example.
    ///
    /// Essentially, this supports:
    ///     `x {>, >=, <, <=} value1 AND x {>, >=, <, <=} value2`.
    pub fn row_ids_filter_range(
        &self,
        left: (T::Native, cmp::Operator),
        right: (T::Native, cmp::Operator),
        bm: Bitmap,
    ) -> Bitmap {
        match (&left.1, &right.1) {
            (cmp::Operator::GT, cmp::Operator::LT)
            | (cmp::Operator::GT, cmp::Operator::LTE)
            | (cmp::Operator::GTE, cmp::Operator::LT)
            | (cmp::Operator::GTE, cmp::Operator::LTE)
            | (cmp::Operator::LT, cmp::Operator::GT)
            | (cmp::Operator::LT, cmp::Operator::GTE)
            | (cmp::Operator::LTE, cmp::Operator::GT)
            | (cmp::Operator::LTE, cmp::Operator::GTE) => self.row_ids_cmp_range_order_bm(
                (left.0, Self::ord_from_op(&left.1)),
                (right.0, Self::ord_from_op(&right.1)),
                bm,
            ),

            (_, _) => panic!("unsupported operators provided"),
        }
    }

    // Special case function for finding all rows that satisfy two operators on
    // two values.
    //
    // This function exists because it is more performant than calling
    // `row_ids_cmp_order_bm` twice and predicates like `WHERE X > y and X <= x`
    // are very common, e.g., for timestamp columns.
    //
    // For performance reasons ranges of matching values are collected up and
    // added in bulk to the bitmap.
    //
    fn row_ids_cmp_range_order_bm(
        &self,
        left: (T::Native, (std::cmp::Ordering, std::cmp::Ordering)),
        right: (T::Native, (std::cmp::Ordering, std::cmp::Ordering)),
        mut bm: Bitmap,
    ) -> Bitmap {
        bm.clear();

        let left_op = left.1;
        let right_op = right.1;

        let mut found = false;
        let mut count = 0;
        for i in 0..self.num_rows() as usize {
            let left_cmp_result = self.arr.value(i).partial_cmp(&left.0);
            let right_cmp_result = self.arr.value(i).partial_cmp(&right.0);

            let left_result_no =
                left_cmp_result != Some(left_op.0) && left_cmp_result != Some(left_op.1);
            let right_result_no =
                right_cmp_result != Some(right_op.0) && left_cmp_result != Some(right_op.1);

            if (self.arr.is_null(i) || left_result_no || right_result_no) && found {
                let (min, max) = (i as u64 - count as u64, i as u64);
                bm.add_range(min..max);
                found = false;
                count = 0;
                continue;
            } else if self.arr.is_null(i) || left_result_no || right_result_no {
                continue;
            }

            if !found {
                found = true;
            }
            count += 1;
        }

        // add any remaining range.
        if found {
            let (min, max) = (self.num_rows() - count as u64, self.num_rows());
            bm.add_range(min..max);
        }
        bm
    }
}

// This macro implements the From trait for slices of various logical types.
//
// Here is an example implementation:
//
//    impl From<&[i64]> for Plain<i16> {
//        fn from(v: &[i64]) -> Self {
//          Self {
//              values: v.to_vec().iter().map(|&x| x as i16).collect(),
//          }
//        }
//    }
//
macro_rules! plain_from_impls {
    ($(($type_from:ty, $type_to:ty),)*) => {
        $(
            impl From<&[$type_from]> for FixedNull<$type_to> {
                fn from(v: &[$type_from]) -> Self {
                    Self{
                        arr: PrimitiveArray::from(v.to_vec()),
                    }
                }
            }

            impl From<&[Option<$type_from>]> for FixedNull<$type_to> {
                fn from(v: &[Option<$type_from>]) -> Self {
                    Self{
                        arr: PrimitiveArray::from(v.to_vec()),
                    }
                }
            }
        )*
    };
}

// Supported logical and physical datatypes for the Plain encoding.
//
// Need to look at possibility of initialising smaller datatypes...
plain_from_impls! {
    (i64, delorean_arrow::arrow::datatypes::Int64Type),
    //  (i64, delorean_arrow::arrow::datatypes::Int32Type),
    //  (i64, delorean_arrow::arrow::datatypes::Int16Type),
    //  (i64, delorean_arrow::arrow::datatypes::Int8Type),
    //  (i64, delorean_arrow::arrow::datatypes::UInt32Type),
    //  (i64, delorean_arrow::arrow::datatypes::UInt16Type),
    //  (i64, delorean_arrow::arrow::datatypes::UInt8Type),
     (i32, delorean_arrow::arrow::datatypes::Int32Type),
    //  (i32, delorean_arrow::arrow::datatypes::Int16Type),
    //  (i32, delorean_arrow::arrow::datatypes::Int8Type),
    //  (i32, delorean_arrow::arrow::datatypes::UInt16Type),
    //  (i32, delorean_arrow::arrow::datatypes::UInt8Type),
     (i16, delorean_arrow::arrow::datatypes::Int16Type),
    //  (i16, delorean_arrow::arrow::datatypes::Int8Type),
    //  (i16, delorean_arrow::arrow::datatypes::UInt8Type),
     (i8, delorean_arrow::arrow::datatypes::Int8Type),
     (u64, delorean_arrow::arrow::datatypes::UInt64Type),
    //  (u64, delorean_arrow::arrow::datatypes::UInt32Type),
    //  (u64, delorean_arrow::arrow::datatypes::UInt16Type),
    //  (u64, delorean_arrow::arrow::datatypes::UInt8Type),
     (u32, delorean_arrow::arrow::datatypes::UInt32Type),
    //  (u32, delorean_arrow::arrow::datatypes::UInt16Type),
    //  (u32, delorean_arrow::arrow::datatypes::UInt8Type),
     (u16, delorean_arrow::arrow::datatypes::UInt16Type),
    //  (u16, delorean_arrow::arrow::datatypes::UInt8Type),
     (u8, delorean_arrow::arrow::datatypes::UInt8Type),
     (f64, delorean_arrow::arrow::datatypes::Float64Type),
}

#[cfg(test)]
mod test {
    use super::cmp::Operator;
    use super::*;
    use delorean_arrow::arrow::datatypes::*;

    fn some_vec<T: Copy>(v: Vec<T>) -> Vec<Option<T>> {
        v.iter().map(|x| Some(*x)).collect()
    }

    #[test]
    fn first_row_id_eq_value() {
        let v = super::FixedNull::<Int64Type>::from(vec![22, 33, 18].as_slice());

        assert_eq!(v.first_row_id_eq_value(33), Some(1));
        assert_eq!(v.first_row_id_eq_value(100), None);
    }

    #[test]
    fn value() {
        let v = super::FixedNull::<Int8Type>::from(vec![22, 33, 18].as_slice());

        assert_eq!(v.value(2), Some(18));
    }

    #[test]
    fn values() {
        let v = super::FixedNull::<Int8Type>::from((0..10).collect::<Vec<_>>().as_slice());

        assert_eq!(v.values(&[0, 1, 2, 3], vec![]), some_vec(vec![0, 1, 2, 3]));
        assert_eq!(
            v.values(&[0, 1, 2, 3, 4], vec![]),
            some_vec(vec![0, 1, 2, 3, 4])
        );
        assert_eq!(
            v.values(&(0..10).collect::<Vec<_>>(), vec![]),
            some_vec(vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9])
        );

        let mut dst = some_vec(vec![22, 33, 44, 55]);
        dst = v.values(&[0, 1], dst);
        assert_eq!(dst, some_vec(vec![0, 1]));
        assert_eq!(dst.capacity(), 4);
    }

    #[test]
    fn all_values() {
        let v = super::FixedNull::<Int8Type>::from((0..10).collect::<Vec<_>>().as_slice());

        assert_eq!(
            v.all_values(vec![]),
            (0..10).map(Some).collect::<Vec<Option<i8>>>()
        );

        let mut dst = some_vec(vec![22, 33, 44, 55]);
        dst = v.all_values(dst);
        assert_eq!(dst, (0..10).map(Some).collect::<Vec<Option<i8>>>());
        assert_eq!(dst.capacity(), 10);
    }

    #[test]
    fn count() {
        let data = vec![Some(0), None, Some(22), None, None, Some(33), Some(44)];
        let v = super::FixedNull::<Int8Type>::from(data.as_slice());

        assert_eq!(v.count(&[0, 1, 2, 3, 4, 5, 6]), 4);
        assert_eq!(v.count(&[1, 3]), 0);
        assert_eq!(v.count(&[6]), 1);
    }

    #[test]
    fn sum() {
        let v = super::FixedNull::<Int8Type>::from((0..10).collect::<Vec<_>>().as_slice());

        assert_eq!(v.sum(&[3, 5, 6, 7]), Some(21));
        assert_eq!(v.sum(&[1, 2, 4, 7, 9]), Some(23));
    }

    #[test]
    fn first() {
        let v = super::FixedNull::<Int16Type>::from((10..20).collect::<Vec<_>>().as_slice());

        assert_eq!(v.first(&[3, 5, 6, 7]), Some(13));
    }

    #[test]
    fn last() {
        let v = super::FixedNull::<Int16Type>::from((10..20).collect::<Vec<_>>().as_slice());

        assert_eq!(v.last(&[3, 5, 6, 7]), Some(17));
    }

    #[test]
    fn min() {
        let v = super::FixedNull::<Int16Type>::from(vec![100, 110, 20, 1, 110].as_slice());

        assert_eq!(v.min(&[0, 1, 2, 3, 4]), Some(1));
    }

    #[test]
    fn max() {
        let v = super::FixedNull::<Int16Type>::from(vec![100, 110, 20, 1, 109].as_slice());

        assert_eq!(v.max(&[0, 1, 2, 3, 4]), Some(110));
    }

    #[test]
    fn row_ids_filter_eq() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::Equal, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 2, 12]);

        let bm = v.row_ids_filter(101, Operator::Equal, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![1, 8]);

        let bm = v.row_ids_filter(2030, Operator::Equal, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![6]);

        let bm = v.row_ids_filter(194, Operator::Equal, Bitmap::create());
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }

    #[test]
    fn row_ids_filter_neq() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::NotEqual, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![1, 3, 4, 5, 6, 7, 8, 9, 10, 11]);

        let bm = v.row_ids_filter(101, Operator::NotEqual, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12]);

        let bm = v.row_ids_filter(2030, Operator::NotEqual, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12]);

        let bm = v.row_ids_filter(194, Operator::NotEqual, Bitmap::create());
        assert_eq!(bm.to_vec(), (0..13).collect::<Vec<u32>>());
    }

    #[test]
    fn row_ids_filter_lt() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::LT, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![7, 9, 10, 11]);

        let bm = v.row_ids_filter(3, Operator::LT, Bitmap::create());
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }

    #[test]
    fn row_ids_filter_lte() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::LTE, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 2, 7, 9, 10, 11, 12]);

        let bm = v.row_ids_filter(2, Operator::LTE, Bitmap::create());
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }

    #[test]
    fn row_ids_filter_gt() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::GT, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![1, 3, 4, 5, 6, 8]);

        let bm = v.row_ids_filter(2030, Operator::GT, Bitmap::create());
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }

    #[test]
    fn row_ids_filter_gte() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![100, 101, 100, 102, 1000, 300, 2030, 3, 101, 4, 5, 21, 100].as_slice(),
        );

        let bm = v.row_ids_filter(100, Operator::GTE, Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 1, 2, 3, 4, 5, 6, 8, 12]);

        let bm = v.row_ids_filter(2031, Operator::GTE, Bitmap::create());
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }

    #[test]
    fn row_ids_filter_range() {
        let v = super::FixedNull::<Int64Type>::from(
            vec![
                Some(100),
                Some(101),
                None,
                None,
                None,
                Some(100),
                Some(102),
                Some(1000),
                Some(300),
                Some(2030),
                None,
                Some(3),
                None,
                Some(101),
                Some(4),
                Some(5),
                Some(21),
                Some(100),
                None,
                None,
            ]
            .as_slice(),
        );

        let bm =
            v.row_ids_filter_range((100, Operator::GTE), (240, Operator::LT), Bitmap::create());
        assert_eq!(bm.to_vec(), vec![0, 1, 5, 6, 13, 17]);

        let bm = v.row_ids_filter_range((100, Operator::GT), (240, Operator::LT), Bitmap::create());
        assert_eq!(bm.to_vec(), vec![1, 6, 13]);

        let bm = v.row_ids_filter_range((10, Operator::LT), (-100, Operator::GT), Bitmap::create());
        assert_eq!(bm.to_vec(), vec![11, 14, 15]);

        let bm = v.row_ids_filter_range((21, Operator::GTE), (21, Operator::LTE), Bitmap::create());
        assert_eq!(bm.to_vec(), vec![16]);

        let bm = v.row_ids_filter_range(
            (10000, Operator::LTE),
            (3999, Operator::GT),
            Bitmap::create(),
        );
        assert_eq!(bm.to_vec(), Vec::<u32>::new());
    }
}