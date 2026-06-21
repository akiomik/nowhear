#![allow(unused_macros)]

macro_rules! debug {
    ($($tt:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            ::tracing::debug!($($tt)*);
        }
    };
}

macro_rules! warn {
    ($($tt:tt)*) => {
        {
            #[cfg(feature = "tracing")]
            ::tracing::warn!($($tt)*);
        }
    };
}
