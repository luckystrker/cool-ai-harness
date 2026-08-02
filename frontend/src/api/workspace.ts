import { api } from "./client"

export interface GitInfo {
  path: string
  is_git: boolean
  branch: string | null
}

export interface DirectoryListing {
  current: string
  parent: string | null
  directories: string[]
  default: string
}

export interface RecentDirectories {
  recent: string[]
  default: string
}

export interface GitStatus {
  path: string
  is_git: boolean
  branch: string | null
  staged: string[]
  modified: string[]
  untracked: string[]
}

export interface GitLogEntry {
  hash: string
  message: string
  author: string
  date: string
}

export interface GitLog {
  path: string
  commits: GitLogEntry[]
}

export interface GitBranches {
  path: string
  branches: string[]
  current: string | null
}

export const workspaceApi = {
  /** Current git branch (if any) for a directory. */
  gitInfo: (path: string) =>
    api.get<GitInfo>(`/api/workspace/git-info?path=${encodeURIComponent(path)}`),

  /** List sub-directories of a path (folder browser). */
  directories: (path?: string) =>
    api.get<DirectoryListing>(
      `/api/workspace/directories${path ? `?path=${encodeURIComponent(path)}` : ""}`
    ),

  /** Recently used working directories + the global default. */
  recent: () => api.get<RecentDirectories>("/api/workspace/recent"),

  /** Parsed git status: branch, staged, modified, untracked files. */
  gitStatus: (path: string) =>
    api.get<GitStatus>(`/api/workspace/git-status?path=${encodeURIComponent(path)}`),

  /** Recent commit log for a repository. */
  gitLog: (path: string, limit = 10) =>
    api.get<GitLog>(
      `/api/workspace/git-log?path=${encodeURIComponent(path)}&limit=${limit}`
    ),

  /** List all local branches and the current one. */
  gitBranches: (path: string) =>
    api.get<GitBranches>(`/api/workspace/git-branches?path=${encodeURIComponent(path)}`),

  /** Switch to a different branch. */
  gitCheckout: (path: string, branch: string) =>
    api.post<{ path: string; branch: string; status: string }>(
      "/api/workspace/git-checkout",
      { path, branch }
    ),
}
