pub mod feed_item;
pub mod subscription;
pub mod tag;

pub use feed_item::*;
pub use subscription::*;

use serde::Deserialize;

/// Serde helper for double-option fields (`Option<Option<T>>`).
///
/// Distinguishes "field absent" (`None`, leave unchanged) from "explicit
/// null" (`Some(None)`, clear the value). Use with `#[serde(default)]` so a
/// missing field deserializes to `None`, and a present field goes through
/// this function.
pub fn de_double_option<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    // This function only runs when the field is present in the payload:
    // null -> Some(None), value -> Some(Some(value))
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}
