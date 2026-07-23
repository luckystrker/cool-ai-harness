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
}
