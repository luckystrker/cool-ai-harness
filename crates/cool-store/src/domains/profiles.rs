//! Agent profiles (legacy `agent_profiles` table, Фаза 3a §2).
//!
//! Profiles are **globally scoped**: the Python `agent_profiles` table has no
//! `user_id` and its CRUD service (`app/agent/personalities/service.py`) is
//! actor-agnostic. These methods therefore take no actor id.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{collect_rows, json_text, parse_json, query_one};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    pub is_builtin: bool,
    pub is_active: bool,
    pub is_shared: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentProfile {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            slug: row.get("slug")?,
            description: row.get("description")?,
            system_prompt: row.get("system_prompt")?,
            model: row.get("model")?,
            tool_names: parse_json(row.get("tool_names")?)?,
            skill_names: parse_json(row.get("skill_names")?)?,
            settings: parse_json(row.get("settings")?)?,
            avatar_color: row.get("avatar_color")?,
            is_builtin: row.get::<_, i64>("is_builtin")? != 0,
            is_active: row.get::<_, i64>("is_active")? != 0,
            is_shared: row.get::<_, i64>("is_shared")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Fields required to create a profile. `is_active` defaults to `true` and the
/// built-in/shared flags default to `false` (Python `create_profile`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewAgentProfile {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    pub is_builtin: bool,
    pub is_active: bool,
    pub is_shared: bool,
}

impl Default for NewAgentProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            slug: String::new(),
            description: None,
            system_prompt: None,
            model: None,
            tool_names: None,
            skill_names: None,
            settings: None,
            avatar_color: None,
            is_builtin: false,
            is_active: true,
            is_shared: false,
        }
    }
}

/// Partial update; `None` means "leave unchanged". `is_builtin` is immutable,
/// matching `update_profile`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfilePatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    /// Empty string clears the model (Python `model or None`).
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub skill_names: Option<Value>,
    /// Empty object clears settings (Python `settings or None`).
    pub settings: Option<Value>,
    pub avatar_color: Option<String>,
    pub is_active: Option<bool>,
    pub is_shared: Option<bool>,
}

/// Fetch a profile without re-locking the store.
fn fetch_profile(connection: &Connection, profile_id: i64) -> Result<AgentProfile, StoreError> {
    query_one(
        connection,
        "SELECT * FROM agent_profiles WHERE id = ?1",
        [profile_id],
        AgentProfile::from_row,
    )?
    .ok_or(StoreError::NotFound("profile"))
}

fn slug_exists(connection: &Connection, slug: &str) -> Result<bool, StoreError> {
    let existing: Option<i64> = query_one(
        connection,
        "SELECT id FROM agent_profiles WHERE slug = ?1",
        [slug],
        |row| Ok(row.get(0)?),
    )?;
    Ok(existing.is_some())
}

/// True when `slug` belongs to a different profile than `profile_id`.
fn slug_taken_by_other(
    connection: &Connection,
    slug: &str,
    profile_id: i64,
) -> Result<bool, StoreError> {
    let existing: Option<i64> = query_one(
        connection,
        "SELECT id FROM agent_profiles WHERE slug = ?1 AND id != ?2",
        params![slug, profile_id],
        |row| Ok(row.get(0)?),
    )?;
    Ok(existing.is_some())
}

/// One built-in profile preset, mirroring `personalities/presets.py`.
struct BuiltinPreset {
    name: &'static str,
    slug: &'static str,
    description: &'static str,
    system_prompt: &'static str,
    avatar_color: &'static str,
    temperature: f64,
    max_iterations: Option<i64>,
}

/// Built-in profile presets, mirroring `personalities/presets.py`.
const BUILTIN_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "Assistant",
        slug: "assistant",
        description: "General-purpose helper for everyday tasks.",
        system_prompt: "You are Assistant, a versatile AI helper. You help users with a wide range of tasks: answering questions, brainstorming, planning, writing, analysis, and light coding.\n\n# Guidelines\n- Be helpful, clear, and concise.\n- Use tools when they improve accuracy (file reading, web search, memory).\n- Adapt your tone to the user's style.\n- When uncertain, ask clarifying questions rather than guessing.\n",
        avatar_color: "#6366F1",
        temperature: 0.7,
        max_iterations: None,
    },
    BuiltinPreset {
        name: "Coder",
        slug: "coder",
        description: "Focused software engineering agent.",
        system_prompt: "You are Coder, a focused software engineering agent. You write, review, debug, and refactor code. You prefer precision over verbosity.\n\n# Guidelines\n- Read existing code before modifying it.\n- Follow the project's conventions, style, and architecture.\n- Write minimal, correct changes - avoid over-engineering.\n- Run tests/linters when available to verify your work.\n- Explain trade-offs briefly when multiple approaches exist.\n- Never introduce security vulnerabilities.\n",
        avatar_color: "#10B981",
        temperature: 0.3,
        max_iterations: Some(15),
    },
    BuiltinPreset {
        name: "Researcher",
        slug: "researcher",
        description: "Deep research and multi-source analysis.",
        system_prompt: "You are Researcher, a deep-research and analysis agent. You gather information from multiple sources, synthesize findings, and present structured conclusions.\n\n# Guidelines\n- Use web_search and web_fetch to find authoritative sources.\n- Cross-reference claims across multiple sources.\n- Cite sources explicitly (URL or title).\n- Structure output with headings, bullet points, and summaries.\n- Distinguish facts from opinions and flag uncertainty.\n- Save important findings to memory for future reference.\n",
        avatar_color: "#F59E0B",
        temperature: 0.5,
        max_iterations: Some(12),
    },
    BuiltinPreset {
        name: "Writer",
        slug: "writer",
        description: "Creative and technical writing specialist.",
        system_prompt: "You are Writer, a creative and technical writing specialist. You craft prose, documentation, articles, stories, and marketing copy with attention to voice and structure.\n\n# Guidelines\n- Match the requested tone, audience, and format precisely.\n- Use vivid language for creative work; precise language for technical work.\n- Structure long pieces with clear headings and logical flow.\n- Offer alternatives when style choices are subjective.\n- Edit ruthlessly: cut filler, strengthen verbs, tighten sentences.\n",
        avatar_color: "#EC4899",
        temperature: 0.9,
        max_iterations: None,
    },
    BuiltinPreset {
        name: "DM",
        slug: "dm",
        description: "Dungeon Master for tabletop RPGs.",
        system_prompt: "You are DM, a Dungeon Master for tabletop role-playing games. You narrate scenes, play NPCs, adjudicate rules, and drive the story forward based on player choices.\n\n# Guidelines\n- Describe scenes vividly: sights, sounds, smells, atmosphere.\n- Play NPCs with distinct voices, motivations, and mannerisms.\n- Present meaningful choices with consequences.\n- Adjudicate actions fairly using the game system's rules.\n- Track inventory, HP, quest state, and NPC relationships via memory tools.\n- Never decide the player's actions for them - present options and wait.\n- Balance combat, exploration, and roleplay.\n",
        avatar_color: "#8B5CF6",
        temperature: 0.85,
        max_iterations: Some(8),
    },
];

impl crate::LegacyStore {
    /// Create any missing built-in profile presets, preserving user edits
    /// (existing slugs are skipped). Mirrors `seed_builtin_profiles`.
    pub fn seed_builtin_profiles(&self) -> Result<u64, StoreError> {
        let mut created = 0;
        for preset in BUILTIN_PRESETS {
            if self.find_profile_by_slug(preset.slug)?.is_some() {
                continue;
            }
            let mut settings = serde_json::Map::new();
            settings.insert(
                "temperature".to_owned(),
                serde_json::Value::from(preset.temperature),
            );
            if let Some(max_iterations) = preset.max_iterations {
                settings.insert(
                    "max_iterations".to_owned(),
                    serde_json::Value::from(max_iterations),
                );
            }
            self.create_profile(&NewAgentProfile {
                name: preset.name.to_owned(),
                slug: preset.slug.to_owned(),
                description: Some(preset.description.to_owned()),
                system_prompt: Some(preset.system_prompt.to_owned()),
                model: None,
                tool_names: None,
                skill_names: None,
                settings: Some(serde_json::Value::Object(settings)),
                avatar_color: Some(preset.avatar_color.to_owned()),
                is_builtin: true,
                is_active: true,
                is_shared: false,
            })?;
            created += 1;
        }
        Ok(created)
    }

    /// List profiles. Built-in presets sort first, then by name; inactive
    /// profiles are hidden unless `include_inactive`.
    pub fn list_profiles(&self, include_inactive: bool) -> Result<Vec<AgentProfile>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM agent_profiles WHERE (?1 = 1 OR is_active = 1) \
             ORDER BY is_builtin DESC, name ASC, id ASC",
        )?;
        let rows = statement.query(params![i64::from(include_inactive)])?;
        collect_rows(rows, AgentProfile::from_row)
    }

    pub fn get_profile(&self, profile_id: i64) -> Result<AgentProfile, StoreError> {
        let connection = self.connection()?;
        fetch_profile(&connection, profile_id)
    }

    pub fn find_profile_by_slug(&self, slug: &str) -> Result<Option<AgentProfile>, StoreError> {
        let connection = self.connection()?;
        query_one(
            &connection,
            "SELECT * FROM agent_profiles WHERE slug = ?1",
            [slug],
            AgentProfile::from_row,
        )
    }

    pub fn create_profile(&self, new: &NewAgentProfile) -> Result<AgentProfile, StoreError> {
        let connection = self.connection()?;
        if slug_exists(&connection, &new.slug)? {
            return Err(StoreError::Conflict(format!(
                "profile slug '{}' already exists",
                new.slug
            )));
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO agent_profiles(created_at, updated_at, name, slug, description,
               system_prompt, model, tool_names, skill_names, settings, avatar_color,
               is_builtin, is_active, is_shared)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                timestamp,
                new.name,
                new.slug,
                new.description,
                new.system_prompt,
                new.model,
                json_text(&new.tool_names)?,
                json_text(&new.skill_names)?,
                json_text(&new.settings)?,
                new.avatar_color,
                i64::from(new.is_builtin),
                i64::from(new.is_active),
                i64::from(new.is_shared),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_profile(id)
    }

    pub fn update_profile(
        &self,
        profile_id: i64,
        patch: &AgentProfilePatch,
    ) -> Result<AgentProfile, StoreError> {
        let connection = self.connection()?;
        fetch_profile(&connection, profile_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(name) = &patch.name {
            assignments.push("name = ?");
            values.push(Box::new(name.clone()));
        }
        if let Some(slug) = &patch.slug {
            if slug_taken_by_other(&connection, slug, profile_id)? {
                return Err(StoreError::Conflict(format!(
                    "profile slug '{slug}' already exists"
                )));
            }
            assignments.push("slug = ?");
            values.push(Box::new(slug.clone()));
        }
        if let Some(description) = &patch.description {
            assignments.push("description = ?");
            values.push(Box::new(description.clone()));
        }
        if let Some(prompt) = &patch.system_prompt {
            assignments.push("system_prompt = ?");
            values.push(Box::new(prompt.clone()));
        }
        if let Some(model) = &patch.model {
            assignments.push("model = ?");
            values.push(Box::new(if model.is_empty() {
                None
            } else {
                Some(model.clone())
            }));
        }
        if patch.tool_names.is_some() {
            assignments.push("tool_names = ?");
            values.push(Box::new(json_text(&patch.tool_names)?));
        }
        if patch.skill_names.is_some() {
            assignments.push("skill_names = ?");
            values.push(Box::new(json_text(&patch.skill_names)?));
        }
        if patch.settings.is_some() {
            assignments.push("settings = ?");
            values.push(Box::new(json_text(&cleared_if_empty(&patch.settings))?));
        }
        if let Some(color) = &patch.avatar_color {
            assignments.push("avatar_color = ?");
            values.push(Box::new(color.clone()));
        }
        if let Some(active) = patch.is_active {
            assignments.push("is_active = ?");
            values.push(Box::new(i64::from(active)));
        }
        if let Some(shared) = patch.is_shared {
            assignments.push("is_shared = ?");
            values.push(Box::new(i64::from(shared)));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(profile_id));
            let sql = format!(
                "UPDATE agent_profiles SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_profile(profile_id)
    }

    /// Delete a non-built-in profile. Built-ins raise [`StoreError::InvalidInput`],
    /// matching the Python `ValueError("Cannot delete a built-in profile")`.
    pub fn delete_profile(&self, profile_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let profile = fetch_profile(&connection, profile_id)?;
        if profile.is_builtin {
            return Err(StoreError::InvalidInput(
                "Cannot delete a built-in profile".to_string(),
            ));
        }
        connection.execute("DELETE FROM agent_profiles WHERE id = ?1", [profile_id])?;
        Ok(())
    }

    /// Copy a profile into a new name/slug. The clone is never built-in or
    /// shared and is active, mirroring the Python clone route.
    pub fn clone_profile(
        &self,
        profile_id: i64,
        new_name: &str,
        new_slug: &str,
    ) -> Result<AgentProfile, StoreError> {
        let connection = self.connection()?;
        let source = fetch_profile(&connection, profile_id)?;
        if slug_exists(&connection, new_slug)? {
            return Err(StoreError::Conflict(format!(
                "profile slug '{new_slug}' already exists"
            )));
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO agent_profiles(created_at, updated_at, name, slug, description,
               system_prompt, model, tool_names, skill_names, settings, avatar_color,
               is_builtin, is_active, is_shared)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 1, 0)",
            params![
                timestamp,
                new_name,
                new_slug,
                source.description.as_deref(),
                source.system_prompt.as_deref(),
                source.model.as_deref(),
                json_text(&source.tool_names)?,
                json_text(&source.skill_names)?,
                json_text(&source.settings)?,
                source.avatar_color.as_deref(),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_profile(id)
    }
}

/// Treat an empty JSON object as `None`, matching Python's `settings or None`.
fn cleared_if_empty(value: &Option<Value>) -> Option<Value> {
    match value {
        Some(Value::Object(map)) if map.is_empty() => None,
        other => other.clone(),
    }
}
