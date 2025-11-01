use alloy::primitives::{I256, U256};
use fastnum::{
    bint,
    decimal::{Context, Decimal, RoundingMode, UnsignedDecimal},
};

/// Fixed-point to decimal converter.
#[derive(Clone, Copy, Debug, Default)]
pub struct Converter {
    decimals: i32,
}

impl Converter {
    pub(crate) fn new(decimals: u8) -> Self {
        Self {
            decimals: decimals as i32,
        }
    }

    pub fn from_unsigned<const N: usize>(&self, value: U256) -> UnsignedDecimal<N> {
        let unscaled = bint::UInt::<N>::from_le_slice(value.as_le_slice())
            .expect("Converter: U256 -> UInt::<N>");
        UnsignedDecimal::<N>::from_parts(
            unscaled,
            -self.decimals,
            Context::default().with_rounding_mode(RoundingMode::Floor),
        )
    }

    pub fn from_signed<const N: usize>(&self, value: I256) -> Decimal<N> {
        let unscaled = bint::UInt::<N>::from_le_slice(value.unsigned_abs().as_le_slice())
            .expect("Converter: abs(I256) -> UInt::<N>");
        Decimal::<N>::from_parts(
            unscaled,
            -self.decimals,
            match value.sign() {
                alloy::primitives::Sign::Negative => fastnum::decimal::Sign::Minus,
                alloy::primitives::Sign::Positive => fastnum::decimal::Sign::Plus,
            },
            Context::default().with_rounding_mode(RoundingMode::Floor),
        )
    }

    pub fn to_unsigned<const N: usize>(&self, value: UnsignedDecimal<N>) -> U256 {
        let rescaled = value.rescale(self.decimals as i16);
        U256::from_le_slice(rescaled.digits().to_radix_le(256).as_slice())
    }

    pub fn to_signed<const N: usize>(&self, value: Decimal<N>) -> I256 {
        let rescaled = value.rescale(self.decimals as i16);
        let mut res = I256::try_from_le_slice(rescaled.digits().to_radix_le(256).as_slice())
            .unwrap_or_default();
        if value.is_negative() {
            res = res.saturating_neg();
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use fastnum::{dec256, udec256};

    use super::*;

    #[test]
    fn test_numeric_converter_from_unsigned() {
        assert_eq!(
            Converter::new(0).from_unsigned(U256::from(1234567890)),
            udec256!(1234567890)
        );
        assert_eq!(
            Converter::new(6).from_unsigned(U256::from(1234567890)),
            udec256!(1234.56789)
        );
        assert_eq!(
            Converter::new(12).from_unsigned(U256::from(1234567890)),
            udec256!(0.00123456789)
        );
    }

    #[test]
    fn test_numeric_converter_from_signed() {
        assert_eq!(
            Converter::new(0).from_signed(I256::try_from(1234567890).unwrap()),
            dec256!(1234567890)
        );
        assert_eq!(
            Converter::new(6).from_signed(I256::try_from(1234567890).unwrap()),
            dec256!(1234.56789)
        );
        assert_eq!(
            Converter::new(12).from_signed(I256::try_from(1234567890).unwrap()),
            dec256!(0.00123456789)
        );

        assert_eq!(
            Converter::new(0).from_signed(I256::try_from(-1234567890).unwrap()),
            dec256!(-1234567890)
        );
        assert_eq!(
            Converter::new(6).from_signed(I256::try_from(-1234567890).unwrap()),
            dec256!(-1234.56789)
        );
        assert_eq!(
            Converter::new(12).from_signed(I256::try_from(-1234567890).unwrap()),
            dec256!(-0.00123456789)
        );
    }

    #[test]
    fn test_numeric_converter_to_unsigned() {
        assert_eq!(
            Converter::new(0).to_unsigned(udec256!(1234567890)),
            U256::from(1234567890)
        );
        assert_eq!(
            Converter::new(6).to_unsigned(udec256!(1234.56789)),
            U256::from(1234567890)
        );
        assert_eq!(
            Converter::new(12).to_unsigned(udec256!(0.00123456789)),
            U256::from(1234567890)
        );
    }

    #[test]
    fn test_numeric_converter_to_signed() {
        assert_eq!(
            Converter::new(0).to_signed(dec256!(1234567890)),
            I256::try_from(1234567890).unwrap(),
        );
        assert_eq!(
            Converter::new(6).to_signed(dec256!(1234.56789)),
            I256::try_from(1234567890).unwrap(),
        );
        assert_eq!(
            Converter::new(12).to_signed(dec256!(0.00123456789)),
            I256::try_from(1234567890).unwrap(),
        );

        assert_eq!(
            Converter::new(0).to_signed(dec256!(-1234567890)),
            I256::try_from(-1234567890).unwrap(),
        );
        assert_eq!(
            Converter::new(6).to_signed(dec256!(-1234.56789)),
            I256::try_from(-1234567890).unwrap(),
        );
        assert_eq!(
            Converter::new(12).to_signed(dec256!(-0.00123456789)),
            I256::try_from(-1234567890).unwrap(),
        );
    }
}
