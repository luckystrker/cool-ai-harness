/** Client-side project management (Фаза — projects MVP).
 *
 * A project groups chats under a shared context: a working folder (local
 * projects) plus optional extra system instructions applied to every agent
 * turn in the project's chats. Projects and the conversation→project mapping
 * are persisted in localStorage — the backend conversation already carries the
 * working_directory, and the project's system instructions are sent per-message
 * (see ChatPage), so no schema change is required for the MVP.
 */

export type ProjectType = "local" | "remote"

export interface Project {
  id: string
  name: string
  type: ProjectType
  /** Filesystem path the agent uses as its working directory (local only). */
  path?: string
  /** Free-form description of the project (shown in its settings). */
  description?: string
  /** Extra system instructions prepended to every turn in this project's chats. */
  systemInstructions?: string
  /** Global skill names disabled for this project. */
  disabledSkills?: string[]
  createdAt: string
}

const PROJECTS_KEY = "harness.projects"
const CONV_PROJECT_KEY = "harness.conversationProject"

// --- projects ---

export function loadProjects(): Project[] {
  try {
    const raw = localStorage.getItem(PROJECTS_KEY)
    if (!raw) return []
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? (parsed as Project[]) : []
  } catch {
    return []
  }
}

export function saveProjects(projects: Project[]): void {
  try {
    localStorage.setItem(PROJECTS_KEY, JSON.stringify(projects))
  } catch {
    /* localStorage unavailable — non-fatal */
  }
}

export function addProject(project: Project): Project[] {
  const projects = [...loadProjects(), project]
  saveProjects(projects)
  return projects
}

export function deleteProject(id: string): Project[] {
  const projects = loadProjects().filter((p) => p.id !== id)
  saveProjects(projects)
  // Drop any conversation mappings that pointed at the removed project.
  const map = loadConversationProjectMap()
  let changed = false
  for (const [convId, pid] of Object.entries(map)) {
    if (pid === id) {
      delete map[convId]
      changed = true
    }
  }
  if (changed) saveConversationProjectMap(map)
  return projects
}

export function getProject(id: string): Project | undefined {
  return loadProjects().find((p) => p.id === id)
}

/**
 * Patch a project's editable settings (name / description / system
 * instructions). Returns the updated project list.
 */
export function updateProject(
  id: string,
  patch: Partial<Pick<Project, "name" | "description" | "systemInstructions" | "disabledSkills">>
): Project[] {
  const projects = loadProjects().map((p) =>
    p.id === id ? { ...p, ...patch } : p
  )
  saveProjects(projects)
  return projects
}

// --- conversation → project mapping ---

type ConvProjectMap = Record<string, string>

export function loadConversationProjectMap(): ConvProjectMap {
  try {
    const raw = localStorage.getItem(CONV_PROJECT_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === "object" ? (parsed as ConvProjectMap) : {}
  } catch {
    return {}
  }
}

export function saveConversationProjectMap(map: ConvProjectMap): void {
  try {
    localStorage.setItem(CONV_PROJECT_KEY, JSON.stringify(map))
  } catch {
    /* non-fatal */
  }
}

export function getProjectIdForConversation(conversationId: number): string | null {
  return loadConversationProjectMap()[String(conversationId)] ?? null
}

export function getProjectForConversation(conversationId: number): Project | undefined {
  const pid = getProjectIdForConversation(conversationId)
  return pid ? getProject(pid) : undefined
}

export function setConversationProject(conversationId: number, projectId: string): void {
  const map = loadConversationProjectMap()
  map[String(conversationId)] = projectId
  saveConversationProjectMap(map)
}

/** Generate a short unique id for a new project. */
export function newProjectId(): string {
  return `proj-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`
}
