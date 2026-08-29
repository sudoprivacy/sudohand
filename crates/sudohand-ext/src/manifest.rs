//! The manifest an extension prints for `--manifest`: enough for `suh ext`
//! to describe it and for an agent to turn its commands into tool
//! definitions. Derived from the clap command tree, so it cannot drift
//! from what the binary actually accepts.

use crate::Platform;
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;

/// What the extension needs from the machine. All optional; declare what
/// applies so `suh ext info` can show it and an integrator can gate on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requires {
    /// OS permissions: `accessibility`, `screen_recording`, …
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// Environment variables (credentials, endpoints).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Bundle ids / app names it drives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
}

impl Requires {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn permissions<I: IntoIterator<Item = S>, S: Into<String>>(mut self, p: I) -> Self {
        self.permissions.extend(p.into_iter().map(Into::into));
        self
    }
    pub fn env<I: IntoIterator<Item = S>, S: Into<String>>(mut self, e: I) -> Self {
        self.env.extend(e.into_iter().map(Into::into));
        self
    }
    pub fn apps<I: IntoIterator<Item = S>, S: Into<String>>(mut self, a: I) -> Self {
        self.apps.extend(a.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub name: String,
    pub description: String,
    pub version: String,
    pub platforms: Vec<Platform>,
    #[serde(default)]
    pub requires: Requires,
    pub commands: Vec<CommandSpec>,
    /// Registered workflows, runnable with `flow <name> --var …`.
    #[serde(default)]
    pub workflows: Vec<WorkflowSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Required `--var` names.
    #[serde(default)]
    pub vars: Vec<String>,
    /// `builtin` (from sudohand-desktop) or the extension's name.
    #[serde(default)]
    pub source: String,
}

impl From<sudohand_flow::Entry> for WorkflowSpec {
    fn from(e: sudohand_flow::Entry) -> Self {
        Self {
            name: e.name,
            description: e.description,
            vars: e.vars,
            source: e.source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub about: String,
    /// `false` for pure queries (`status`, `list`); `true` otherwise.
    pub mutates: bool,
    #[serde(default)]
    pub args: Vec<ArgSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    pub name: String,
    /// `flag` (`--name value`) or `positional`.
    pub kind: ArgKind,
    /// `string` / `int` / `float` / `bool` / `path` / `enum`.
    #[serde(rename = "type")]
    pub ty: ArgType,
    pub required: bool,
    /// May be given more than once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgKind {
    Flag,
    Positional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    String,
    Int,
    Float,
    Bool,
    Path,
    Enum,
}

impl Manifest {
    pub fn from_command(
        name: &str,
        version: &str,
        platforms: &[Platform],
        requires: Requires,
        readonly: &[&str],
        cmd: &clap::Command,
        workflows: Vec<sudohand_flow::Entry>,
    ) -> Self {
        let commands = cmd
            .get_subcommands()
            .filter(|c| c.get_name() != "help")
            .map(|c| CommandSpec {
                name: c.get_name().to_string(),
                about: c.get_about().map(|s| s.to_string()).unwrap_or_default(),
                mutates: !readonly.contains(&c.get_name()),
                args: c
                    .get_arguments()
                    .filter(|a| !matches!(a.get_id().as_str(), "help" | "version"))
                    .map(arg_spec)
                    .collect(),
            })
            .collect();
        Self {
            schema: SCHEMA,
            name: name.to_string(),
            description: cmd.get_about().map(|s| s.to_string()).unwrap_or_default(),
            version: version.to_string(),
            platforms: platforms.to_vec(),
            requires,
            commands,
            workflows: workflows.into_iter().map(Into::into).collect(),
        }
    }
}

fn arg_spec(a: &clap::Arg) -> ArgSpec {
    use clap::builder::ArgAction as A;
    let action = a.get_action();
    let kind = if a.get_long().is_some() || a.get_short().is_some() {
        ArgKind::Flag
    } else {
        ArgKind::Positional
    };
    let values: Vec<String> = a
        .get_possible_values()
        .iter()
        .map(|v| v.get_name().to_string())
        .collect();
    let ty = match action {
        A::SetTrue | A::SetFalse => ArgType::Bool,
        A::Count => ArgType::Int,
        _ if !values.is_empty() => ArgType::Enum,
        _ => value_type(a.get_value_parser()),
    };
    let default = a
        .get_default_values()
        .first()
        .map(|v| v.to_string_lossy().into_owned());
    ArgSpec {
        name: a.get_id().to_string(),
        kind,
        ty,
        required: a.is_required_set(),
        multiple: matches!(action, A::Append),
        values,
        default,
        about: a.get_help().map(|s| s.to_string()).unwrap_or_default(),
    }
}

/// Map the parser's produced type onto our coarse `ArgType`. `AnyValueId`
/// is not nameable outside clap, but two of them can be compared, so we
/// build reference parsers and match by identity.
fn value_type(p: &clap::builder::ValueParser) -> ArgType {
    use clap::builder::ValueParser as V;
    use clap::value_parser as vp;
    let id = p.type_id();
    let is = |q: V| q.type_id() == id;
    if [
        V::from(vp!(i8)),
        V::from(vp!(i16)),
        V::from(vp!(i32)),
        V::from(vp!(i64)),
        V::from(vp!(u8)),
        V::from(vp!(u16)),
        V::from(vp!(u32)),
        V::from(vp!(u64)),
        V::from(vp!(usize)),
        V::from(vp!(isize)),
    ]
    .into_iter()
    .any(is)
    {
        ArgType::Int
    } else if [V::from(vp!(f32)), V::from(vp!(f64))].into_iter().any(is) {
        ArgType::Float
    } else if is(V::path_buf()) {
        ArgType::Path
    } else if is(V::bool()) {
        ArgType::Bool
    } else {
        ArgType::String
    }
}
