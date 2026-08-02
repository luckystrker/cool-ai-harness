"""Tests for the skills system (Фаза 2 §3)."""

from __future__ import annotations

from pathlib import Path

import pytest

from app.skills.context import build_skills_context
from app.skills.matching import (
    cosine_similarity,
    keyword_score,
    rank_skills,
    select_relevant_skills,
    tokenize,
)
from app.skills.models import Skill, parse_skill_md
from app.skills.registry import SkillRegistry, get_skill_registry, reset_skill_registry

# --- parse_skill_md ---


class TestParseSkillMd:
    def test_no_front_matter(self):
        text = "# My Skill\n\nDo something cool."
        meta, body = parse_skill_md(text)
        assert meta == {}
        assert body == "# My Skill\n\nDo something cool."

    def test_with_front_matter(self):
        text = "---\nname: test-skill\ndescription: A test skill\nversion: \"2.0\"\n---\n\n# Body\n\nInstructions here."
        meta, body = parse_skill_md(text)
        assert meta["name"] == "test-skill"
        assert meta["description"] == "A test skill"
        assert meta["version"] == "2.0"
        assert body == "# Body\n\nInstructions here."

    def test_list_items(self):
        text = "---\nname: multi\ntags:\n  - alpha\n  - beta\n  - gamma\n---\n\nBody."
        meta, body = parse_skill_md(text)
        assert meta["name"] == "multi"
        assert meta["tags"] == ["alpha", "beta", "gamma"]
        assert body == "Body."

    def test_inline_comma_list(self):
        text = "---\ntags: foo, bar, baz\n---\nBody."
        meta, _body = parse_skill_md(text)
        assert meta["tags"] == ["foo", "bar", "baz"]

    def test_boolean_values(self):
        text = "---\nenabled: true\ndebug: false\n---\nBody."
        meta, _ = parse_skill_md(text)
        assert meta["enabled"] is True
        assert meta["debug"] is False

    def test_integer_value(self):
        text = "---\npriority: 5\n---\nBody."
        meta, _ = parse_skill_md(text)
        assert meta["priority"] == 5

    def test_quoted_string(self):
        text = '---\nname: "my-skill"\n---\nBody.'
        meta, _ = parse_skill_md(text)
        assert meta["name"] == "my-skill"


# --- Skill dataclass ---


class TestSkill:
    def test_from_directory(self, tmp_path: Path):
        skill_dir = tmp_path / "my-skill"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text(
            "---\nname: my-skill\ndescription: Test skill\ntags:\n  - test\n  - demo\n---\n\n# Instructions\n\nDo the thing.",
            encoding="utf-8",
        )
        skill = Skill.from_directory(skill_dir)
        assert skill is not None
        assert skill.name == "my-skill"
        assert skill.description == "Test skill"
        assert skill.tags == ["test", "demo"]
        assert "Do the thing." in skill.body
        assert skill.source == "builtin"

    def test_from_directory_no_skill_md(self, tmp_path: Path):
        skill_dir = tmp_path / "empty"
        skill_dir.mkdir()
        assert Skill.from_directory(skill_dir) is None

    def test_from_directory_custom_source(self, tmp_path: Path):
        skill_dir = tmp_path / "user-skill"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text("---\nname: user-skill\n---\nBody.", encoding="utf-8")
        skill = Skill.from_directory(skill_dir, source="user")
        assert skill is not None
        assert skill.source == "user"

    def test_context_block(self, tmp_path: Path):
        skill = Skill(
            name="test",
            description="A test",
            body="Instructions here.",
            path=tmp_path,
        )
        block = skill.context_block()
        assert "# Skill: test" in block
        assert "A test" in block
        assert "Instructions here." in block

    def test_list_resources(self, tmp_path: Path):
        skill_dir = tmp_path / "res-skill"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text("---\nname: res\n---\nBody.", encoding="utf-8")
        (skill_dir / "template.txt").write_text("template", encoding="utf-8")
        (skill_dir / "helper.py").write_text("# helper", encoding="utf-8")
        (skill_dir / ".hidden").write_text("hidden", encoding="utf-8")

        skill = Skill.from_directory(skill_dir)
        assert skill is not None
        resources = skill.list_resources()
        names = [r.name for r in resources]
        assert "template.txt" in names
        assert "helper.py" in names
        assert ".hidden" not in names
        assert "SKILL.md" not in names

    def test_name_fallback_to_dir_name(self, tmp_path: Path):
        skill_dir = tmp_path / "fallback-name"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text("No front matter here.", encoding="utf-8")
        skill = Skill.from_directory(skill_dir)
        assert skill is not None
        assert skill.name == "fallback-name"


# --- SkillRegistry ---


class TestSkillRegistry:
    def test_load_from_directory(self, tmp_path: Path):
        # Create a skill structure.
        skill_dir = tmp_path / "test-skill"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text(
            "---\nname: test-skill\ndescription: For testing\n---\nBody.",
            encoding="utf-8",
        )

        registry = SkillRegistry()
        registry._load_from_dir(tmp_path, source="user", target={})
        # Use internal method to test loading.
        skills: dict[str, Skill] = {}
        registry._load_from_dir(tmp_path, source="user", target=skills)
        assert "test-skill" in skills
        assert skills["test-skill"].source == "user"

    def test_register_and_get(self):
        registry = SkillRegistry()
        registry._loaded = True  # Skip filesystem loading.
        skill = Skill(name="manual", description="Manual skill", body="Body.", path=Path("."))
        registry.register(skill)
        assert registry.get("manual") is skill
        assert registry.count == 1

    def test_unregister(self):
        registry = SkillRegistry()
        registry._loaded = True
        skill = Skill(name="temp", description="Temp", body="Body.", path=Path("."))
        registry.register(skill)
        assert registry.unregister("temp") is True
        assert registry.get("temp") is None
        assert registry.unregister("nonexistent") is False

    def test_clear(self):
        registry = SkillRegistry()
        registry._loaded = True
        registry.register(Skill(name="a", description="", body="", path=Path(".")))
        registry.clear()
        # After clear, _loaded is False so internal dict is empty.
        assert len(registry._skills) == 0
        assert registry._loaded is False

    def test_list_by_source(self):
        registry = SkillRegistry()
        registry._loaded = True
        registry.register(Skill(name="b1", description="", body="", path=Path("."), source="builtin"))
        registry.register(Skill(name="u1", description="", body="", path=Path("."), source="user"))
        assert len(registry.list_by_source("builtin")) == 1
        assert len(registry.list_by_source("user")) == 1
        assert len(registry.list_by_source("plugin")) == 0

    def test_names_sorted(self):
        registry = SkillRegistry()
        registry._loaded = True
        registry.register(Skill(name="zeta", description="", body="", path=Path(".")))
        registry.register(Skill(name="alpha", description="", body="", path=Path(".")))
        assert registry.names() == ["alpha", "zeta"]

    def test_skips_hidden_dirs(self, tmp_path: Path):
        hidden = tmp_path / ".hidden"
        hidden.mkdir()
        (hidden / "SKILL.md").write_text("---\nname: hidden\n---\nBody.", encoding="utf-8")
        visible = tmp_path / "visible"
        visible.mkdir()
        (visible / "SKILL.md").write_text("---\nname: visible\n---\nBody.", encoding="utf-8")

        registry = SkillRegistry()
        skills: dict[str, Skill] = {}
        registry._load_from_dir(tmp_path, source="builtin", target=skills)
        assert "visible" in skills
        assert "hidden" not in skills


# --- Matching ---


class TestTokenize:
    def test_basic(self):
        tokens = tokenize("Hello World! This is a Test.")
        assert "hello" in tokens
        assert "world" in tokens
        assert "test" in tokens
        # Stop words removed.
        assert "this" not in tokens
        assert "is" not in tokens
        assert "a" not in tokens

    def test_hyphenated(self):
        tokens = tokenize("deep-research task")
        assert "deep-research" in tokens
        assert "task" in tokens


class TestKeywordScore:
    def _make_skill(self, name: str, description: str = "", tags: list[str] | None = None) -> Skill:
        return Skill(
            name=name,
            description=description,
            body="Some body text.",
            path=Path("."),
            tags=tags or [],
        )

    def test_exact_name_match(self):
        skill = self._make_skill("deep-research", "Research things")
        score = keyword_score("deep research", skill)
        assert score > 0.5

    def test_tag_match(self):
        skill = self._make_skill("dr", "Do stuff", tags=["research", "analysis"])
        score = keyword_score("research analysis", skill)
        assert score > 0.3

    def test_no_match(self):
        skill = self._make_skill("translate", "Translate text", tags=["language"])
        score = keyword_score("quantum physics simulation", skill)
        assert score < 0.15

    def test_empty_query(self):
        skill = self._make_skill("test")
        assert keyword_score("", skill) == 0.0


class TestCosineSimilarity:
    def test_identical(self):
        assert cosine_similarity([1.0, 0.0], [1.0, 0.0]) == pytest.approx(1.0)

    def test_orthogonal(self):
        assert cosine_similarity([1.0, 0.0], [0.0, 1.0]) == pytest.approx(0.0)

    def test_zero_vector(self):
        assert cosine_similarity([0.0, 0.0], [1.0, 1.0]) == 0.0

    def test_different_lengths(self):
        assert cosine_similarity([1.0], [1.0, 2.0]) == 0.0


class TestRankSkills:
    def _skills(self) -> list[Skill]:
        return [
            Skill(name="deep-research", description="Research topics thoroughly", body="Research instructions.", path=Path("."), tags=["research", "analysis"]),
            Skill(name="code-task", description="Implement code changes", body="Code instructions.", path=Path("."), tags=["code", "programming"]),
            Skill(name="translate", description="Translate between languages", body="Translate instructions.", path=Path("."), tags=["translation", "language"]),
        ]

    def test_ranks_relevant_first(self):
        results = rank_skills("research quantum computing", self._skills())
        assert len(results) > 0
        assert results[0].skill.name == "deep-research"

    def test_threshold_filters(self):
        results = rank_skills("completely unrelated xyz", self._skills(), threshold=0.5)
        assert len(results) == 0

    def test_with_embeddings(self):
        skills = self._skills()
        # Simulate embeddings where query is close to "code-task".
        query_emb = [0.9, 0.1, 0.0]
        skill_embs = {
            "deep-research": [0.1, 0.9, 0.0],
            "code-task": [0.85, 0.15, 0.0],
            "translate": [0.0, 0.0, 1.0],
        }
        results = rank_skills(
            "implement feature",
            skills,
            query_embedding=query_emb,
            skill_embeddings=skill_embs,
            threshold=0.1,
        )
        # code-task should rank high due to embedding similarity.
        code_results = [r for r in results if r.skill.name == "code-task"]
        assert len(code_results) > 0
        assert code_results[0].embedding_score is not None


class TestSelectRelevantSkills:
    def test_max_skills(self):
        skills = [
            Skill(name=f"skill-{i}", description=f"Skill number {i}", body="Body.", path=Path("."), tags=[f"tag{i}"])
            for i in range(10)
        ]
        result = select_relevant_skills("skill number", skills, max_skills=3)
        assert len(result) == 3  # exactly max_skills when enough candidates exist


# --- Context injection ---


class TestBuildSkillsContext:
    def setup_method(self):
        reset_skill_registry()

    def teardown_method(self):
        reset_skill_registry()

    def test_returns_none_when_no_skills(self):
        registry = get_skill_registry()
        registry._loaded = True
        registry.clear()
        registry._loaded = True
        assert build_skills_context("anything") is None

    def test_returns_context_with_skills(self):
        registry = get_skill_registry()
        registry._loaded = True
        registry.register(
            Skill(name="deep-research", description="Research topics", body="Body.", path=Path("."), tags=["research"])
        )
        ctx = build_skills_context("research quantum computing")
        assert ctx is not None
        assert "deep-research" in ctx
        assert "Available Skills" in ctx
        assert "use_skill" in ctx


# --- Skill tools integration ---


class TestSkillTools:
    async def test_list_skills_tool(self):
        from app.tools.skill_tools import _list_skills

        reset_skill_registry()
        registry = get_skill_registry()
        registry._loaded = True
        registry.register(
            Skill(name="test-skill", description="A test", body="Body.", path=Path("."), source="builtin")
        )
        result = await _list_skills()
        assert not result.is_error
        assert "test-skill" in result.output
        reset_skill_registry()

    async def test_use_skill_tool(self):
        from app.tools.skill_tools import _use_skill

        reset_skill_registry()
        registry = get_skill_registry()
        registry._loaded = True
        registry.register(
            Skill(name="my-skill", description="My skill", body="Do the thing.", path=Path("."))
        )
        result = await _use_skill(name="my-skill")
        assert not result.is_error
        assert "Do the thing." in result.output
        assert "my-skill" in result.output
        reset_skill_registry()

    async def test_use_skill_not_found(self):
        from app.tools.skill_tools import _use_skill

        reset_skill_registry()
        registry = get_skill_registry()
        registry._loaded = True
        result = await _use_skill(name="nonexistent")
        assert result.is_error
        assert "not found" in result.output
        reset_skill_registry()
