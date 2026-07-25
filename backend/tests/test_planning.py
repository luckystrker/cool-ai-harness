"""Tests for Planning Mode (Фаза 2 §1).

Covers:
- Plan extraction from agent responses
- Plan CRUD (create, edit steps, approve, cancel)
- Plan context injection (build_plan_context)
- plan_step_update tool
- Plan templates CRUD
- API endpoint integration tests
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session

from app.agent.planning import (
    approve_plan,
    build_plan_context,
    cancel_plan,
    create_plan,
    create_template,
    delete_template,
    extract_plan_from_response,
    get_plan,
    get_plan_steps,
    list_plans,
    list_templates,
    update_plan_steps,
)
from app.core.db import engine
from app.main import app
from app.models.plan import (
    PLAN_STATUS_APPROVED,
    PLAN_STATUS_CANCELLED,
    PLAN_STATUS_COMPLETED,
    PLAN_STATUS_DRAFT,
    PLAN_STATUS_EXECUTING,
    STEP_STATUS_COMPLETED,
    STEP_STATUS_PENDING,
)

client = TestClient(app)


@pytest.fixture(autouse=True)
def _ensure_tables():
    """Ensure all tables exist (uses SQLModel.create_all via init_db)."""
    from app.core.db import init_db

    init_db()


@pytest.fixture
def session():
    with Session(engine) as s:
        yield s


@pytest.fixture
def conv_id(session: Session) -> int:
    """Create a test conversation and return its id."""
    from app.agent.service import create_conversation, get_or_create_default_user

    user = get_or_create_default_user(session)
    conv = create_conversation(session, user_id=user.id, title="Plan test")
    return conv.id


SAMPLE_STEPS = [
    {"position": 0, "title": "Research", "description": "Gather info", "depends_on": [], "tools": ["web_search"]},
    {"position": 1, "title": "Implement", "description": "Write code", "depends_on": [0], "tools": ["write_file"]},
    {"position": 2, "title": "Verify", "description": "Run tests", "depends_on": [1], "tools": ["python_execute"]},
]


# --- Unit tests: planning service ---


class TestPlanCRUD:
    def test_create_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Test plan", steps=SAMPLE_STEPS)
        assert plan.id is not None
        assert plan.title == "Test plan"
        assert plan.status == PLAN_STATUS_DRAFT
        assert plan.conversation_id == conv_id

    def test_get_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Fetch me", steps=SAMPLE_STEPS)
        fetched = get_plan(session, plan.id)
        assert fetched is not None
        assert fetched.title == "Fetch me"

    def test_list_plans(self, session: Session, conv_id: int):
        create_plan(session, conversation_id=conv_id, title="Plan A", steps=SAMPLE_STEPS)
        create_plan(session, conversation_id=conv_id, title="Plan B", steps=SAMPLE_STEPS)
        plans = list_plans(session, conversation_id=conv_id)
        assert len(plans) >= 2
        # Newest first.
        assert plans[0].title == "Plan B"

    def test_get_plan_steps(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Steps", steps=SAMPLE_STEPS)
        steps = get_plan_steps(session, plan.id)
        assert len(steps) == 3
        assert steps[0].title == "Research"
        assert steps[0].status == STEP_STATUS_PENDING
        assert steps[1].depends_on == [0]

    def test_update_plan_steps(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Edit me", steps=SAMPLE_STEPS)
        new_steps = [
            {"position": 0, "title": "New step 1", "description": "Updated"},
            {"position": 1, "title": "New step 2", "description": "Also updated"},
        ]
        updated = update_plan_steps(session, plan.id, title="Edited plan", steps=new_steps)
        assert updated is not None
        assert updated.title == "Edited plan"
        steps = get_plan_steps(session, plan.id)
        assert len(steps) == 2
        assert steps[0].title == "New step 1"

    def test_update_plan_only_in_draft(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Approve first", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)
        result = update_plan_steps(session, plan.id, title="Should fail")
        assert result is None

    def test_approve_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Approve me", steps=SAMPLE_STEPS)
        approved = approve_plan(session, plan.id)
        assert approved is not None
        assert approved.status == PLAN_STATUS_APPROVED

    def test_approve_plan_not_draft(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Already approved", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)
        # Second approval should fail.
        result = approve_plan(session, plan.id)
        assert result is None

    def test_cancel_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Cancel me", steps=SAMPLE_STEPS)
        cancelled = cancel_plan(session, plan.id)
        assert cancelled is not None
        assert cancelled.status == PLAN_STATUS_CANCELLED

    def test_cancel_terminal_plan_fails(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Terminal", steps=SAMPLE_STEPS)
        cancel_plan(session, plan.id)
        # Already cancelled — second cancel should fail.
        result = cancel_plan(session, plan.id)
        assert result is None


class TestExtractPlanFromResponse:
    def test_valid_plan_block(self):
        content = 'Here is my research...\n\n```plan\n{"title": "My Plan", "steps": [{"position": 0, "title": "Step A"}]}\n```'
        result = extract_plan_from_response(content)
        assert result is not None
        assert result["title"] == "My Plan"
        assert len(result["steps"]) == 1

    def test_missing_plan_block(self):
        content = "Just a regular response with no plan block."
        result = extract_plan_from_response(content)
        assert result is None

    def test_malformed_json_in_block(self):
        content = "```plan\n{not valid json}\n```"
        result = extract_plan_from_response(content)
        assert result is None

    def test_missing_steps_key(self):
        content = '```plan\n{"title": "No steps"}\n```'
        result = extract_plan_from_response(content)
        assert result is None

    def test_empty_steps_array(self):
        content = '```plan\n{"title": "Empty", "steps": []}\n```'
        result = extract_plan_from_response(content)
        assert result is None

    def test_normalizes_positions(self):
        content = '```plan\n{"title": "T", "steps": [{"title": "A"}, {"title": "B"}]}\n```'
        result = extract_plan_from_response(content)
        assert result is not None
        assert result["steps"][0]["position"] == 0
        assert result["steps"][1]["position"] == 1


class TestBuildPlanContext:
    def test_no_active_plan(self, session: Session, conv_id: int):
        result = build_plan_context(session, conv_id)
        assert result is None

    def test_approved_plan_shows_context(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Test Plan", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)
        ctx = build_plan_context(session, conv_id)
        assert ctx is not None
        assert "Active Plan: Test Plan" in ctx
        assert "plan_step_update" in ctx
        assert "[ ]" in ctx  # pending steps

    def test_draft_plan_not_shown(self, session: Session, conv_id: int):
        create_plan(session, conversation_id=conv_id, title="Draft", steps=SAMPLE_STEPS)
        result = build_plan_context(session, conv_id)
        assert result is None


class TestPlanStepUpdateTool:
    async def test_update_step_completed(self, session: Session, conv_id: int):
        from app.tools.plan_tools import _plan_step_update

        plan = create_plan(session, conversation_id=conv_id, title="Tool test", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)

        result = await _plan_step_update(plan_id=plan.id, position=0, status="completed", summary="Done")
        assert not result.is_error
        assert "completed" in result.output

        # Expire cached objects so we read fresh state from the DB.
        session.expire_all()

        # Verify DB state.
        steps = get_plan_steps(session, plan.id)
        assert steps[0].status == STEP_STATUS_COMPLETED
        assert steps[0].result_summary == "Done"

        # Plan should transition to executing.
        refreshed = get_plan(session, plan.id)
        assert refreshed.status == PLAN_STATUS_EXECUTING

    async def test_update_invalid_plan(self):
        from app.tools.plan_tools import _plan_step_update

        result = await _plan_step_update(plan_id=99999, position=0, status="completed")
        assert result.is_error
        assert "not found" in result.output

    async def test_update_invalid_position(self, session: Session, conv_id: int):
        from app.tools.plan_tools import _plan_step_update

        plan = create_plan(session, conversation_id=conv_id, title="Pos test", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)

        result = await _plan_step_update(plan_id=plan.id, position=99, status="completed")
        assert result.is_error
        assert "not found" in result.output

    async def test_all_steps_complete_finalizes_plan(self, session: Session, conv_id: int):
        from app.tools.plan_tools import _plan_step_update

        steps = [{"position": 0, "title": "Only step"}]
        plan = create_plan(session, conversation_id=conv_id, title="Finalize", steps=steps)
        approve_plan(session, plan.id)

        await _plan_step_update(plan_id=plan.id, position=0, status="completed", summary="All done")

        # Expire cached objects so we read fresh state from the DB.
        session.expire_all()

        refreshed = get_plan(session, plan.id)
        assert refreshed.status == PLAN_STATUS_COMPLETED


class TestPlanTemplates:
    def test_create_template(self, session: Session):
        tpl = create_template(session, name="Research", description="Deep research", steps=SAMPLE_STEPS)
        assert tpl.id is not None
        assert tpl.name == "Research"
        assert tpl.is_builtin is False

    def test_list_templates(self, session: Session):
        create_template(session, name="T1", steps=[])
        create_template(session, name="T2", steps=[])
        templates = list_templates(session)
        assert len(templates) >= 2

    def test_delete_template(self, session: Session):
        tpl = create_template(session, name="Deletable", steps=[])
        assert delete_template(session, tpl.id) is True
        assert delete_template(session, tpl.id) is False  # already gone

    def test_delete_builtin_template_fails(self, session: Session):
        tpl = create_template(session, name="Built-in", steps=[], is_builtin=True)
        assert delete_template(session, tpl.id) is False


# --- API integration tests ---


class TestPlansAPI:
    def test_list_plans_empty(self, conv_id: int):
        resp = client.get(f"/api/conversations/{conv_id}/plans")
        assert resp.status_code == 200
        assert resp.json() == []

    def test_list_plans_404_conversation(self):
        resp = client.get("/api/conversations/99999/plans")
        assert resp.status_code == 404

    def test_get_plan_detail(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="API plan", steps=SAMPLE_STEPS)
        resp = client.get(f"/api/conversations/{conv_id}/plans/{plan.id}")
        assert resp.status_code == 200
        data = resp.json()
        assert data["title"] == "API plan"
        assert data["status"] == "draft"
        assert len(data["steps"]) == 3

    def test_patch_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Patch me", steps=SAMPLE_STEPS)
        resp = client.patch(
            f"/api/conversations/{conv_id}/plans/{plan.id}",
            json={"title": "Patched", "steps": [{"position": 0, "title": "Only step"}]},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["title"] == "Patched"
        assert len(data["steps"]) == 1

    def test_patch_plan_not_draft(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Approved", steps=SAMPLE_STEPS)
        approve_plan(session, plan.id)
        resp = client.patch(
            f"/api/conversations/{conv_id}/plans/{plan.id}",
            json={"title": "Should fail"},
        )
        assert resp.status_code == 400

    def test_approve_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Approve API", steps=SAMPLE_STEPS)
        resp = client.post(
            f"/api/conversations/{conv_id}/plans/{plan.id}/approve",
            json={"approved": True},
        )
        assert resp.status_code == 200
        assert resp.json()["status"] == "approved"

    def test_reject_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Reject API", steps=SAMPLE_STEPS)
        resp = client.post(
            f"/api/conversations/{conv_id}/plans/{plan.id}/approve",
            json={"approved": False},
        )
        assert resp.status_code == 200
        assert resp.json()["status"] == "cancelled"

    def test_cancel_plan(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Cancel API", steps=SAMPLE_STEPS)
        resp = client.post(f"/api/conversations/{conv_id}/plans/{plan.id}/cancel")
        assert resp.status_code == 200
        assert resp.json()["status"] == "cancelled"

    def test_execute_plan_not_approved(self, session: Session, conv_id: int):
        plan = create_plan(session, conversation_id=conv_id, title="Not approved", steps=SAMPLE_STEPS)
        resp = client.post(f"/api/conversations/{conv_id}/plans/{plan.id}/execute")
        assert resp.status_code == 400


class TestPlanTemplatesAPI:
    def test_list_templates_empty(self):
        resp = client.get("/api/plan-templates")
        assert resp.status_code == 200

    def test_create_template(self):
        resp = client.post(
            "/api/plan-templates",
            json={"name": "Code Review", "description": "Review code", "steps": SAMPLE_STEPS},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["name"] == "Code Review"
        assert data["is_builtin"] is False

    def test_delete_template(self):
        resp = client.post("/api/plan-templates", json={"name": "Temp", "steps": []})
        tpl_id = resp.json()["id"]
        resp = client.delete(f"/api/plan-templates/{tpl_id}")
        assert resp.status_code == 200
        assert resp.json()["deleted"] == tpl_id

    def test_delete_nonexistent_template(self):
        resp = client.delete("/api/plan-templates/99999")
        assert resp.status_code == 404
