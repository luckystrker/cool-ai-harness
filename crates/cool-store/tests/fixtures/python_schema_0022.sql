-- Baseline schema snapshot for the Rust legacy store (crates/cool-store).
-- Generated from the real Alembic migration chain; do not edit by hand.
-- Regenerate: backend $ python -m tests.schema_snapshot --update <this file>
-- Alembic revision: 0022
-- Excludes the sqlite-vec `memory_vec` table and FTS5 shadow tables:
-- SQLite rebuilds the FTS shadow tables from the virtual table DDL, and
-- the Rust store must keep working on databases without sqlite-vec.
CREATE TABLE alembic_version (
	version_num VARCHAR(32) NOT NULL, 
	CONSTRAINT alembic_version_pkc PRIMARY KEY (version_num)
);
CREATE TABLE users (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	external_id VARCHAR, 
	username VARCHAR, 
	display_name VARCHAR, 
	is_active BOOLEAN NOT NULL, 
	PRIMARY KEY (id)
);
CREATE TABLE messages (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	role VARCHAR NOT NULL, 
	content TEXT, 
	tool_calls JSON, 
	tool_result JSON, 
	usage JSON, 
	thinking TEXT, model TEXT, duration_ms INTEGER, artifact_ids JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id)
);
CREATE INDEX ix_messages_conversation_id ON messages (conversation_id);
CREATE TABLE tool_calls (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER, 
	message_id INTEGER, 
	user_id INTEGER, 
	name VARCHAR NOT NULL, 
	arguments JSON, 
	result JSON, 
	duration_ms INTEGER, 
	success BOOLEAN NOT NULL, 
	error TEXT, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(message_id) REFERENCES messages (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_tool_calls_conversation_id ON tool_calls (conversation_id);
CREATE INDEX ix_tool_calls_message_id ON tool_calls (message_id);
CREATE INDEX ix_tool_calls_user_id ON tool_calls (user_id);
CREATE TABLE agent_runs (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	user_id INTEGER, 
	status VARCHAR NOT NULL, 
	model VARCHAR, 
	config JSON, 
	checkpoint JSON, 
	usage JSON, 
	iterations INTEGER NOT NULL, 
	finish_reason VARCHAR, 
	error TEXT, 
	started_at DATETIME NOT NULL, 
	finished_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_agent_runs_conversation_id ON agent_runs (conversation_id);
CREATE INDEX ix_agent_runs_status ON agent_runs (status);
CREATE INDEX ix_agent_runs_user_id ON agent_runs (user_id);
CREATE TABLE run_events (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	run_id INTEGER NOT NULL, 
	seq INTEGER NOT NULL, 
	kind VARCHAR NOT NULL, 
	payload JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX ix_run_events_run_id ON run_events (run_id);
CREATE TABLE approval_audits (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	run_id INTEGER, 
	call_id VARCHAR NOT NULL, 
	tool_name VARCHAR NOT NULL, 
	arguments JSON, 
	approved BOOLEAN NOT NULL, 
	decision_source VARCHAR NOT NULL, 
	decided_by VARCHAR, 
	reason TEXT, 
	is_breakpoint BOOLEAN NOT NULL, 
	breakpoint_type VARCHAR, 
	duration_ms INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX ix_approval_audits_conversation_id ON approval_audits (conversation_id);
CREATE INDEX ix_approval_audits_run_id ON approval_audits (run_id);
CREATE TABLE artifacts (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	run_id INTEGER, 
	tool_call_id VARCHAR, 
	filename VARCHAR NOT NULL, 
	media_type VARCHAR NOT NULL, 
	kind VARCHAR NOT NULL, 
	size_bytes INTEGER NOT NULL, 
	sha256 VARCHAR, 
	storage_path VARCHAR NOT NULL, 
	version INTEGER NOT NULL, 
	parent_id INTEGER, 
	metadata_ JSON, 
	extracted_text TEXT, 
	is_deleted BOOLEAN NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id), 
	FOREIGN KEY(parent_id) REFERENCES artifacts (id)
);
CREATE INDEX ix_artifacts_conversation_id ON artifacts (conversation_id);
CREATE INDEX ix_artifacts_run_id ON artifacts (run_id);
CREATE INDEX ix_artifacts_kind ON artifacts (kind);
CREATE INDEX ix_artifacts_sha256 ON artifacts (sha256);
CREATE INDEX ix_artifacts_is_deleted ON artifacts (is_deleted);
CREATE TABLE budgets (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	daily_limit_usd FLOAT, 
	weekly_limit_usd FLOAT, 
	monthly_limit_usd FLOAT, 
	alert_threshold_pct FLOAT NOT NULL, 
	block_on_exceed BOOLEAN NOT NULL, 
	override_until DATETIME, 
	last_alert_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_budgets_user_id ON budgets (user_id);
CREATE TABLE spend_log (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	run_id INTEGER, 
	conversation_id INTEGER, 
	provider_name VARCHAR NOT NULL, 
	model VARCHAR NOT NULL, 
	prompt_tokens INTEGER NOT NULL, 
	completion_tokens INTEGER NOT NULL, 
	total_tokens INTEGER NOT NULL, 
	cost_usd FLOAT NOT NULL, 
	ts DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id)
);
CREATE INDEX ix_spend_log_user_id ON spend_log (user_id);
CREATE INDEX ix_spend_log_run_id ON spend_log (run_id);
CREATE INDEX ix_spend_log_conversation_id ON spend_log (conversation_id);
CREATE INDEX ix_spend_log_ts ON spend_log (ts);
CREATE INDEX ix_spend_log_user_ts ON spend_log (user_id, ts);
CREATE TABLE "providers" (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	label VARCHAR, 
	base_url VARCHAR, 
	api_key_encrypted VARCHAR, 
	default_model VARCHAR, 
	is_active BOOLEAN NOT NULL, 
	is_subscription BOOLEAN NOT NULL, 
	is_fallback BOOLEAN DEFAULT 0 NOT NULL, 
	chat_models JSON, 
	is_default BOOLEAN DEFAULT 0 NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_providers_user_id ON providers (user_id);
CREATE INDEX ix_providers_is_fallback ON providers (is_fallback);
CREATE INDEX ix_providers_is_default ON providers (is_default);
CREATE TABLE plans (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	run_id INTEGER, 
	title VARCHAR, 
	status VARCHAR NOT NULL, 
	steps JSON, 
	metadata_ JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX ix_plans_conversation_id ON plans (conversation_id);
CREATE INDEX ix_plans_run_id ON plans (run_id);
CREATE INDEX ix_plans_status ON plans (status);
CREATE TABLE plan_steps (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	plan_id INTEGER NOT NULL, 
	position INTEGER NOT NULL, 
	title VARCHAR NOT NULL, 
	description TEXT, 
	status VARCHAR NOT NULL, 
	depends_on JSON, 
	tools JSON, 
	result_summary TEXT, 
	run_id INTEGER, delegate_role VARCHAR, 
	PRIMARY KEY (id), 
	FOREIGN KEY(plan_id) REFERENCES plans (id)
);
CREATE INDEX ix_plan_steps_plan_id ON plan_steps (plan_id);
CREATE TABLE plan_templates (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT, 
	steps JSON NOT NULL, 
	is_builtin BOOLEAN NOT NULL, 
	PRIMARY KEY (id)
);
CREATE TABLE subagent_roles (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT, 
	system_prompt TEXT, 
	model VARCHAR, 
	tool_names JSON, 
	capability_policy JSON, 
	max_iterations INTEGER DEFAULT '10' NOT NULL, 
	max_cost_usd FLOAT, 
	is_builtin BOOLEAN DEFAULT 0 NOT NULL, 
	PRIMARY KEY (id)
);
CREATE INDEX ix_subagent_roles_name ON subagent_roles (name);
CREATE TABLE memory_items (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	scope VARCHAR DEFAULT 'global' NOT NULL, 
	agent_id INTEGER, 
	conversation_id INTEGER, 
	memory_type VARCHAR DEFAULT 'semantic' NOT NULL, 
	content TEXT NOT NULL, 
	structured JSON, 
	tags JSON, 
	importance FLOAT DEFAULT '0.5' NOT NULL, 
	confidence FLOAT DEFAULT '0.7' NOT NULL, 
	source VARCHAR DEFAULT 'agent' NOT NULL, 
	status VARCHAR DEFAULT 'active' NOT NULL, 
	supersedes_id INTEGER, 
	access_count INTEGER DEFAULT '0' NOT NULL, 
	last_accessed_at DATETIME, 
	ttl_days INTEGER, 
	valid_from DATETIME, 
	valid_to DATETIME, pinned BOOLEAN DEFAULT 0 NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id)
);
CREATE INDEX ix_memory_items_user_id ON memory_items (user_id);
CREATE INDEX ix_memory_items_scope ON memory_items (scope);
CREATE INDEX ix_memory_items_agent_id ON memory_items (agent_id);
CREATE INDEX ix_memory_items_memory_type ON memory_items (memory_type);
CREATE INDEX ix_memory_items_status ON memory_items (status);
CREATE TABLE episodes (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	agent_id INTEGER, 
	conversation_id INTEGER, 
	run_id INTEGER, 
	title VARCHAR NOT NULL, 
	summary TEXT NOT NULL, 
	outcome VARCHAR DEFAULT 'unknown' NOT NULL, 
	importance FLOAT DEFAULT '0.5' NOT NULL, 
	tags JSON, 
	related_entities JSON, 
	started_at DATETIME, 
	ended_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX ix_episodes_user_id ON episodes (user_id);
CREATE INDEX ix_episodes_agent_id ON episodes (agent_id);
CREATE TABLE working_memory (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	conversation_id INTEGER NOT NULL, 
	state JSON DEFAULT '{}' NOT NULL, 
	summary TEXT, 
	summary_up_to_message_id INTEGER, 
	token_estimate INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	UNIQUE (conversation_id)
);
CREATE INDEX ix_working_memory_conversation_id ON working_memory (conversation_id);
CREATE VIRTUAL TABLE memory_fts USING fts5(
            content,
            tags,
            memory_type UNINDEXED,
            content='memory_items',
            content_rowid='id'
        );
CREATE TRIGGER memory_items_ai AFTER INSERT ON memory_items BEGIN
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END;
CREATE TRIGGER memory_items_ad AFTER DELETE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
        END;
CREATE TRIGGER memory_items_au AFTER UPDATE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END;
CREATE INDEX ix_memory_items_pinned ON memory_items (pinned);
CREATE TABLE entities (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	entity_type VARCHAR DEFAULT 'concept' NOT NULL, 
	aliases JSON, 
	attributes JSON, 
	description TEXT, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	CONSTRAINT uq_entities_user_id_name UNIQUE (user_id, name)
);
CREATE INDEX ix_entities_user_id ON entities (user_id);
CREATE INDEX ix_entities_name ON entities (name);
CREATE INDEX ix_entities_entity_type ON entities (entity_type);
CREATE TABLE entity_relations (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	source_entity_id INTEGER NOT NULL, 
	target_entity_id INTEGER NOT NULL, 
	relation_type VARCHAR DEFAULT 'related_to' NOT NULL, 
	attributes JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(source_entity_id) REFERENCES entities (id), 
	FOREIGN KEY(target_entity_id) REFERENCES entities (id)
);
CREATE INDEX ix_entity_relations_user_id ON entity_relations (user_id);
CREATE INDEX ix_entity_relations_source_entity_id ON entity_relations (source_entity_id);
CREATE INDEX ix_entity_relations_target_entity_id ON entity_relations (target_entity_id);
CREATE TABLE memory_item_entities (
	memory_id INTEGER NOT NULL, 
	entity_id INTEGER NOT NULL, 
	PRIMARY KEY (memory_id, entity_id), 
	FOREIGN KEY(memory_id) REFERENCES memory_items (id), 
	FOREIGN KEY(entity_id) REFERENCES entities (id)
);
CREATE TABLE "conversations" (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	title VARCHAR, 
	provider VARCHAR, 
	model VARCHAR, 
	working_directory VARCHAR, 
	permissions JSON, 
	metadata_ JSON, 
	capability_policy JSON, 
	profile_id INTEGER, tags JSON, folder VARCHAR, is_pinned BOOLEAN DEFAULT '0' NOT NULL, is_archived BOOLEAN DEFAULT '0' NOT NULL, 
	PRIMARY KEY (id), 
	CONSTRAINT fk_conversations_profile_id FOREIGN KEY(profile_id) REFERENCES agent_profiles (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_conversations_user_id ON conversations (user_id);
CREATE INDEX ix_conversations_profile_id ON conversations (profile_id);
CREATE TABLE wiki_articles (
	id INTEGER NOT NULL, 
	title VARCHAR NOT NULL, 
	content TEXT DEFAULT '' NOT NULL, 
	category VARCHAR DEFAULT 'general' NOT NULL, 
	tags JSON, 
	source VARCHAR DEFAULT 'manual' NOT NULL, 
	source_memory_id INTEGER, 
	user_id INTEGER, 
	project_key VARCHAR, 
	is_pinned BOOLEAN DEFAULT '0' NOT NULL, 
	is_archived BOOLEAN DEFAULT '0' NOT NULL, 
	version INTEGER DEFAULT '1' NOT NULL, 
	metadata_ JSON, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_wiki_articles_title ON wiki_articles (title);
CREATE INDEX ix_wiki_articles_category ON wiki_articles (category);
CREATE INDEX ix_wiki_articles_user_id ON wiki_articles (user_id);
CREATE INDEX ix_wiki_articles_project_key ON wiki_articles (project_key);
CREATE INDEX ix_wiki_articles_is_archived ON wiki_articles (is_archived);
CREATE INDEX ix_conversations_folder ON conversations (folder);
CREATE INDEX ix_conversations_is_pinned ON conversations (is_pinned);
CREATE INDEX ix_conversations_is_archived ON conversations (is_archived);
CREATE TABLE scheduled_tasks (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT, 
	trigger_type VARCHAR NOT NULL, 
	cron_expression VARCHAR, 
	interval_seconds INTEGER, 
	run_at DATETIME, 
	timezone VARCHAR NOT NULL, 
	quiet_hours_start VARCHAR, 
	quiet_hours_end VARCHAR, 
	misfire_policy VARCHAR NOT NULL, 
	prompt TEXT NOT NULL, 
	workflow_type VARCHAR, 
	profile_id INTEGER, 
	model VARCHAR, 
	tools_whitelist JSON, 
	capability_policy JSON, 
	working_directory VARCHAR, 
	approval_policy VARCHAR NOT NULL, 
	delivery_channels JSON, 
	delivery_config JSON, 
	last_delivery_hash VARCHAR, 
	max_iterations INTEGER NOT NULL, 
	max_cost_per_run FLOAT, 
	timeout_s FLOAT, 
	enabled BOOLEAN NOT NULL, 
	next_run_at DATETIME, 
	last_run_at DATETIME, 
	last_status VARCHAR, 
	run_count INTEGER NOT NULL, 
	failure_count INTEGER NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(profile_id) REFERENCES agent_profiles (id)
);
CREATE INDEX ix_scheduled_tasks_user_id ON scheduled_tasks (user_id);
CREATE INDEX ix_scheduled_tasks_name ON scheduled_tasks (name);
CREATE INDEX ix_scheduled_tasks_trigger_type ON scheduled_tasks (trigger_type);
CREATE INDEX ix_scheduled_tasks_profile_id ON scheduled_tasks (profile_id);
CREATE INDEX ix_scheduled_tasks_enabled ON scheduled_tasks (enabled);
CREATE INDEX ix_scheduled_tasks_next_run_at ON scheduled_tasks (next_run_at);
CREATE TABLE task_runs (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	conversation_id INTEGER, 
	run_id INTEGER, 
	status VARCHAR NOT NULL, 
	trigger_source VARCHAR NOT NULL, 
	prompt TEXT NOT NULL, 
	output TEXT, 
	error TEXT, 
	skip_reason VARCHAR, 
	approval_policy VARCHAR, 
	approval_reason TEXT, 
	usage JSON, 
	duration_ms INTEGER, 
	delivery_status JSON, 
	delivered_at DATETIME, 
	is_read BOOLEAN NOT NULL, 
	started_at DATETIME NOT NULL, 
	finished_at DATETIME, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES scheduled_tasks (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX ix_task_runs_task_id ON task_runs (task_id);
CREATE INDEX ix_task_runs_status ON task_runs (status);
CREATE INDEX ix_task_runs_is_read ON task_runs (is_read);
CREATE INDEX ix_run_events_run_id_kind ON run_events (run_id, kind);
CREATE INDEX ix_run_events_created_at ON run_events (created_at);
CREATE INDEX ix_tool_calls_created_at_success ON tool_calls (created_at, success);
CREATE INDEX ix_memory_items_conversation_created ON memory_items (conversation_id, created_at);
CREATE INDEX ix_memory_items_user_status ON memory_items (user_id, status);
CREATE INDEX ix_episodes_conversation_id ON episodes (conversation_id);
CREATE INDEX ix_spend_log_user_created ON spend_log (user_id, created_at);
CREATE INDEX ix_task_runs_is_read_created ON task_runs (is_read, created_at);
CREATE TABLE rss_subscriptions (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	url VARCHAR NOT NULL, 
	title VARCHAR, 
	site_url VARCHAR, 
	category VARCHAR, 
	fetch_interval_minutes INTEGER NOT NULL, 
	enabled BOOLEAN NOT NULL, 
	last_fetched_at DATETIME, 
	last_error TEXT, 
	entry_count INTEGER NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_rss_subscriptions_user_id ON rss_subscriptions (user_id);
CREATE INDEX ix_rss_subscriptions_url ON rss_subscriptions (url);
CREATE INDEX ix_rss_subscriptions_category ON rss_subscriptions (category);
CREATE INDEX ix_rss_subscriptions_enabled ON rss_subscriptions (enabled);
CREATE TABLE rss_entries (
	id INTEGER NOT NULL, 
	subscription_id INTEGER NOT NULL, 
	guid VARCHAR NOT NULL, 
	title VARCHAR, 
	link VARCHAR, 
	author VARCHAR, 
	summary TEXT, 
	published_at DATETIME, 
	content_hash VARCHAR, 
	is_read BOOLEAN NOT NULL, 
	fetched_at DATETIME NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(subscription_id) REFERENCES rss_subscriptions (id)
);
CREATE INDEX ix_rss_entries_subscription_id ON rss_entries (subscription_id);
CREATE INDEX ix_rss_entries_guid ON rss_entries (guid);
CREATE INDEX ix_rss_entries_published_at ON rss_entries (published_at);
CREATE INDEX ix_rss_entries_content_hash ON rss_entries (content_hash);
CREATE INDEX ix_rss_entries_is_read ON rss_entries (is_read);
CREATE TABLE webhook_endpoints (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	hook_id VARCHAR NOT NULL, 
	secret VARCHAR NOT NULL, 
	source_type VARCHAR NOT NULL, 
	event_filter JSON, 
	task_id INTEGER, 
	prompt_template TEXT, 
	enabled BOOLEAN NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id), 
	FOREIGN KEY(task_id) REFERENCES scheduled_tasks (id)
);
CREATE INDEX ix_webhook_endpoints_user_id ON webhook_endpoints (user_id);
CREATE INDEX ix_webhook_endpoints_name ON webhook_endpoints (name);
CREATE UNIQUE INDEX ix_webhook_endpoints_hook_id ON webhook_endpoints (hook_id);
CREATE INDEX ix_webhook_endpoints_source_type ON webhook_endpoints (source_type);
CREATE INDEX ix_webhook_endpoints_enabled ON webhook_endpoints (enabled);
CREATE TABLE webhook_events (
	id INTEGER NOT NULL, 
	endpoint_id INTEGER NOT NULL, 
	event_type VARCHAR, 
	payload JSON, 
	signature_valid BOOLEAN NOT NULL, 
	status VARCHAR NOT NULL, 
	task_run_id INTEGER, 
	error TEXT, 
	received_at DATETIME NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(endpoint_id) REFERENCES webhook_endpoints (id), 
	FOREIGN KEY(task_run_id) REFERENCES task_runs (id)
);
CREATE INDEX ix_webhook_events_endpoint_id ON webhook_events (endpoint_id);
CREATE INDEX ix_webhook_events_event_type ON webhook_events (event_type);
CREATE INDEX ix_webhook_events_status ON webhook_events (status);
CREATE TABLE research_runs (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	conversation_id INTEGER, 
	parent_task_run_id INTEGER, 
	topic TEXT, 
	depth INTEGER NOT NULL, 
	model VARCHAR, 
	status VARCHAR NOT NULL, 
	sub_questions JSON, 
	sources JSON, 
	citations JSON, 
	report_markdown TEXT, 
	report_artifact_id INTEGER, 
	usage JSON, 
	error TEXT, 
	input_hash VARCHAR, 
	finished_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(parent_task_run_id) REFERENCES task_runs (id), 
	FOREIGN KEY(report_artifact_id) REFERENCES artifacts (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_research_runs_conversation_id ON research_runs (conversation_id);
CREATE INDEX ix_research_runs_input_hash ON research_runs (input_hash);
CREATE INDEX ix_research_runs_parent_task_run_id ON research_runs (parent_task_run_id);
CREATE INDEX ix_research_runs_report_artifact_id ON research_runs (report_artifact_id);
CREATE INDEX ix_research_runs_status ON research_runs (status);
CREATE INDEX ix_research_runs_user_id ON research_runs (user_id);
CREATE TABLE memory_embeddings (
	id INTEGER NOT NULL, 
	memory_id INTEGER NOT NULL, 
	model VARCHAR DEFAULT '' NOT NULL, 
	dimension INTEGER DEFAULT '1536' NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(memory_id) REFERENCES memory_items (id)
);
CREATE UNIQUE INDEX ix_memory_embeddings_memory_id ON memory_embeddings (memory_id);
CREATE TABLE "agent_profiles" (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	slug VARCHAR NOT NULL, 
	description TEXT, 
	system_prompt TEXT, 
	model VARCHAR, 
	tool_names JSON, 
	skill_names JSON, 
	settings JSON, 
	avatar_color VARCHAR, 
	is_builtin BOOLEAN DEFAULT 0 NOT NULL, 
	is_active BOOLEAN DEFAULT 1 NOT NULL, 
	is_shared BOOLEAN DEFAULT 0 NOT NULL, 
	PRIMARY KEY (id), 
	CONSTRAINT uq_agent_profiles_slug UNIQUE (slug)
);
CREATE INDEX ix_agent_profiles_name ON agent_profiles (name);
CREATE INDEX ix_agent_profiles_is_active ON agent_profiles (is_active);
CREATE INDEX ix_agent_profiles_slug ON agent_profiles (slug);
CREATE INDEX ix_agent_profiles_is_shared ON agent_profiles (is_shared);
CREATE TABLE "subagent_runs" (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	role_id INTEGER, 
	parent_conversation_id INTEGER NOT NULL, 
	parent_run_id INTEGER, 
	conversation_id INTEGER NOT NULL, 
	run_id INTEGER, 
	name VARCHAR, 
	prompt TEXT NOT NULL, 
	status VARCHAR DEFAULT 'queued' NOT NULL, 
	result_summary TEXT, 
	usage JSON, 
	error TEXT, 
	started_at DATETIME NOT NULL, 
	finished_at DATETIME, 
	profile_id INTEGER, 
	research_run_id INTEGER, 
	PRIMARY KEY (id), 
	CONSTRAINT fk_subagent_runs_profile_id FOREIGN KEY(profile_id) REFERENCES agent_profiles (id), 
	CONSTRAINT fk_subagent_runs_research_run_id_research_runs FOREIGN KEY(research_run_id) REFERENCES research_runs (id), 
	FOREIGN KEY(run_id) REFERENCES agent_runs (id), 
	FOREIGN KEY(parent_run_id) REFERENCES agent_runs (id), 
	FOREIGN KEY(conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(parent_conversation_id) REFERENCES conversations (id), 
	FOREIGN KEY(role_id) REFERENCES subagent_roles (id)
);
CREATE INDEX ix_subagent_runs_parent_conversation_id ON subagent_runs (parent_conversation_id);
CREATE INDEX ix_subagent_runs_status ON subagent_runs (status);
CREATE INDEX ix_subagent_runs_parent_run_id ON subagent_runs (parent_run_id);
CREATE INDEX ix_subagent_runs_profile_id ON subagent_runs (profile_id);
CREATE INDEX ix_subagent_runs_role_id ON subagent_runs (role_id);
CREATE INDEX ix_subagent_runs_research_run_id ON subagent_runs (research_run_id);
CREATE TABLE macro_tools (
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	name VARCHAR NOT NULL, 
	description TEXT NOT NULL, 
	input_schema JSON NOT NULL, 
	steps JSON NOT NULL, 
	is_active BOOLEAN NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id)
);
CREATE INDEX ix_macro_tools_user_id ON macro_tools (user_id);
CREATE UNIQUE INDEX ix_macro_tools_name ON macro_tools (name);
CREATE INDEX ix_macro_tools_is_active ON macro_tools (is_active);
INSERT INTO alembic_version(version_num) VALUES ('0022');
