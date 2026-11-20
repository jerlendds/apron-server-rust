use serde_json::{Map, Value};

use crate::wire::{Error, Params, Result, object};

pub(crate) fn merge(target: &mut Value, params: &Params, limit: Option<usize>) -> Result<()> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let Some(patch) = object(params, "ext")? else {
        return Ok(());
    };
    let mut extension = target["ext"].as_object().cloned().unwrap_or_else(Map::new);
    for (key, value) in patch {
        if key.is_empty() || key.starts_with("ext:") {
            return Err(Error::Invalid);
        }
        if value.as_str() == Some("")
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(Map::is_empty)
        {
            extension.remove(key);
        } else {
            extension.insert(key.clone(), value.clone());
        }
    }
    let extension = Value::Object(extension);
    if extension.to_string().len() > limit {
        return Err(Error::TooLarge);
    }
    if extension.as_object().is_some_and(Map::is_empty) {
        target.as_object_mut().ok_or(Error::Internal)?.remove("ext");
    } else {
        target["ext"] = extension;
    }
    Ok(())
}
