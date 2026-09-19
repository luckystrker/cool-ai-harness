import { idempotencyKey, sdk } from "./sdk"

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
  gitInfo: async (path: string): Promise<GitInfo> => {
    const record = await sdk.workspaceGitInfo({ path })
    return { path: record.path, is_git: record.isGit, branch: record.branch }
  },

  /** List sub-directories of a path (folder browser). */
  directories: async (path?: string): Promise<DirectoryListing> =>
    sdk.workspaceDirectories({ path: path ?? null }),

  /** Recently used working directories + the global default. */
  recent: async (): Promise<RecentDirectories> => sdk.workspaceRecent({}),

  /** Parsed git status: branch, staged, modified, untracked files. */
  gitStatus: async (path: string): Promise<GitStatus> => {
    const record = await sdk.workspaceGitStatus({ path })
    return {
      path: record.path,
      is_git: record.isGit,
      branch: record.branch,
      staged: record.staged,
      modified: record.modified,
      untracked: record.untracked,
    }
  },

  /** Recent commit log for a repository. */
  gitLog: async (path: string, limit = 10): Promise<GitLog> =>
    sdk.workspaceGitLog({ path, limit }),

  /** List all local branches and the current one. */
  gitBranches: async (path: string): Promise<GitBranches> =>
    sdk.workspaceGitBranches({ path }),

  /** Switch to a different branch. */
  gitCheckout: async (path: string, branch: string) =>
    sdk.workspaceGitCheckout({ idempotencyKey: idempotencyKey(), path, branch }),
}
