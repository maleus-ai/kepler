//! Shared Lua VM setup for all Kepler Lua contexts.
//!
//! Provides [`create_lua_vm`] which builds a base Lua (Luau) state with:
//! - `require` removed
//! - `json` and `yaml` stdlib (parse/stringify, frozen)
//! - Standard library tables frozen (`string`, `math`, `table`, etc.)
//!
//! Callers then customize the VM for their specific use case (config eval,
//! ACL authorizers, etc.) by adding or removing globals.

use mlua::prelude::*;

/// Create a base Lua VM with common Kepler setup.
///
/// The returned VM has:
/// - `require` removed (no external module loading)
/// - `json.parse` / `json.stringify` (frozen)
/// - `yaml.parse` / `yaml.stringify` (frozen)
/// - Standard library tables frozen (`string`, `math`, `table`, `coroutine`,
///   `bit32`, `utf8`, `buffer`, `os`)
///
/// Callers should further customize the VM (e.g., remove `io`/`debug`,
/// restrict `os`, add domain-specific globals) before use.
pub fn create_lua_vm() -> LuaResult<Lua> {
    let lua = Lua::new();
    let globals = lua.globals();

    // Remove `require` — no external module loading
    globals.set("require", LuaValue::Nil)?;

    register_json_stdlib(&lua)?;
    register_yaml_stdlib(&lua)?;
    freeze_stdlib(&lua)?;

    Ok(lua)
}

/// Register `json.parse` and `json.stringify` (frozen table).
fn register_json_stdlib(lua: &Lua) -> LuaResult<()> {
    let json_table = lua.create_table()?;
    json_table.set(
        "parse",
        lua.create_function(|lua, s: String| {
            let v: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| mlua::Error::RuntimeError(format!("json.parse: {}", e)))?;
            lua.to_value(&v)
        })?,
    )?;
    json_table.set(
        "stringify",
        lua.create_function(|lua, (val, pretty): (LuaValue, Option<bool>)| {
            let v: serde_json::Value = lua.from_value(val)?;
            if pretty.unwrap_or(false) {
                serde_json::to_string_pretty(&v)
            } else {
                serde_json::to_string(&v)
            }
            .map_err(|e| mlua::Error::RuntimeError(format!("json.stringify: {}", e)))
        })?,
    )?;
    json_table.set_readonly(true);
    lua.globals().set("json", json_table)?;
    Ok(())
}

/// Register `yaml.parse` and `yaml.stringify` (frozen table).
fn register_yaml_stdlib(lua: &Lua) -> LuaResult<()> {
    let yaml_table = lua.create_table()?;
    yaml_table.set(
        "parse",
        lua.create_function(|lua, s: String| {
            let v: serde_yaml::Value = serde_yaml::from_str(&s)
                .map_err(|e| mlua::Error::RuntimeError(format!("yaml.parse: {}", e)))?;
            lua.to_value(&v)
        })?,
    )?;
    yaml_table.set(
        "stringify",
        lua.create_function(|lua, val: LuaValue| {
            let v: serde_yaml::Value = lua.from_value(val)?;
            serde_yaml::to_string(&v)
                .map_err(|e| mlua::Error::RuntimeError(format!("yaml.stringify: {}", e)))
        })?,
    )?;
    yaml_table.set_readonly(true);
    lua.globals().set("yaml", yaml_table)?;
    Ok(())
}

/// Freeze standard library tables to prevent tampering.
fn freeze_stdlib(lua: &Lua) -> LuaResult<()> {
    for name in &[
        "string", "math", "table", "coroutine", "bit32", "utf8", "buffer", "os",
    ] {
        if let Ok(tbl) = lua.globals().get::<LuaTable>(*name) {
            tbl.set_readonly(true);
        }
    }
    Ok(())
}
