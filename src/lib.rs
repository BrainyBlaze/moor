#[doc(hidden)]
#[macro_export]
macro_rules! schema {
    (map fn $name:ident($key:ident: $kind:ty) -> $value:ty; $($pattern:literal => $result:expr),+ $(,)?) => {
        fn $name($key: $kind) -> ::core::option::Option<$value> { ::core::option::Option::Some(match $key { $($pattern => $result,)+ _ => return ::core::option::Option::None }) }
    };
    (struct default $vis:vis $name:ident $field_vis:vis fields; $($field:ident: $kind:ty = $value:expr),* $(,)?) => {
        $vis struct $name { $($field_vis $field: $kind),* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $value),* } }
        }
    };
    (struct default $vis:vis $name:ident derive [$($derive:ident),+] $field_vis:vis fields; $($field:ident: $kind:ty = $value:expr),* $(,)?) => {
        #[derive($($derive),+)] $vis struct $name { $($field_vis $field: $kind),* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $value),* } }
        }
    };
    (struct $vis:vis $name:ident<$generic:tt> $field_vis:vis fields; $($field:ident: $kind:ty),* $(,)?) => {
        $vis struct $name<$generic> { $($field_vis $field: $kind),* }
    };
    (struct $vis:vis $name:ident derive [$($derive:ident),+] $field_vis:vis fields; $($field:ident: $kind:ty),* $(,)?) => {
        #[derive($($derive),+)] $vis struct $name { $($field_vis $field: $kind),* }
    };
    (struct $vis:vis $name:ident $field_vis:vis fields; $($field:ident: $kind:ty),* $(,)?) => {
        $vis struct $name { $($field_vis $field: $kind),* }
    };
    (tuple $vis:vis $name:ident $(<$generic:lifetime>)? [$($derive:ident),+]; fields pub; $($kind:ty),+ $(,)?) => {
        #[derive($($derive),+)] $vis struct $name $(<$generic>)? ($(pub $kind),+);
    };
    (tuple $vis:vis $name:ident $(<$generic:lifetime>)? [$($derive:ident),+]; fields; $($kind:ty),+ $(,)?) => {
        #[derive($($derive),+)] $vis struct $name $(<$generic>)? ($($kind),+);
    };
    (enum ordinal $vis:vis $name:ident; $($variant:ident),+ $(,)?) => {
        #[repr(u8)] #[derive(Clone, Copy, Debug, Eq, PartialEq)] $vis enum $name { $($variant),+ }
        #[allow(dead_code)] impl $name { fn from_ordinal(value: u8) -> Self { [$(Self::$variant),+][value as usize] } }
    };
    (enum $vis:vis $name:ident $(<$generic:lifetime>)? $([$($derive:ident),+])?; $($(#[$meta:meta])* $variant:ident $(($($kind:ty),* $(,)?))? $({$($field:ident: $field_kind:ty),* $(,)?})?),+ $(,)?) => {
        $(#[derive($($derive),+)])? $vis enum $name $(<$generic>)? { $($(#[$meta])* $variant $(($($kind),*))? $({$($field: $field_kind),*})?),+ }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! binary_record {
    ($raw:ident => $model:ident[$size:literal] error $error_type:ty = $error:expr;
        fixed { $($fixed:ident: $fixed_kind:ty = $fixed_value:expr),* $(,)? }
        fields { $($field:ident: $kind:ty),* $(,)? }) => {
        #[repr(C)]
        #[derive(Clone, Copy, zerocopy::FromBytes, zerocopy::Immutable, zerocopy::IntoBytes, zerocopy::KnownLayout)]
        struct $raw { $($fixed: $fixed_kind,)* $($field: $kind),* }
        const _: [(); $size] = [(); std::mem::size_of::<$raw>()];
        impl $model {
            fn encode_raw(self) -> [u8; $size] {
                let raw = $raw { $($fixed: $fixed_value,)* $($field: self.$field.into()),* };
                zerocopy::IntoBytes::as_bytes(&raw).try_into().expect("record size")
            }
            fn decode_raw(bytes: &[u8]) -> std::result::Result<Self, $error_type> {
                let raw = <$raw as zerocopy::FromBytes>::read_from_bytes(bytes).map_err(|_| $error)?;
                if false $(|| raw.$fixed != $fixed_value)* { return Err($error); }
                Ok(Self { $($field: raw.$field.into()),* })
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! return_if {
    ($condition:expr $(, $value:expr)?) => {
        if $condition {
            return $($value)?;
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! ensure {
    ($valid:expr, $message:expr) => {
        if !$valid {
            return Err(($message).into());
        }
    };
}

pub(crate) fn require(valid: bool, message: &str) -> Result<(), String> {
    valid.then_some(()).ok_or_else(|| message.into())
}

pub(crate) fn protocol(error: impl std::fmt::Debug) -> String {
    format!("protocol error: {error:?}")
}

pub(crate) fn canonical_u64(text: &str) -> Option<u64> {
    crate::wire::decimal(text.as_bytes(), u64::MAX, true)
}

pub mod cli;
pub mod events;
pub mod name;
pub mod runtime;
pub mod session;
pub mod store;
pub mod terminal;
pub mod unix;
pub mod wire;
