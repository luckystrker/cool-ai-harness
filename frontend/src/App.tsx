import { lazy, Suspense } from "react"
import { Routes, Route, Navigate } from "react-router-dom"
import { AppLayout } from "@/components/layout/AppLayout"
import { ChatPage } from "@/pages/ChatPage"

const AnalyticsPage = lazy(() =>
  import("@/pages/AnalyticsPage").then((module) => ({ default: module.AnalyticsPage }))
)
const DeepResearchPage = lazy(() =>
  import("@/pages/DeepResearchPage").then((module) => ({ default: module.DeepResearchPage }))
)
const InspectorPage = lazy(() =>
  import("@/pages/InspectorPage").then((module) => ({ default: module.InspectorPage }))
)
const MemoryPage = lazy(() =>
  import("@/pages/MemoryPage").then((module) => ({ default: module.MemoryPage }))
)
const MobilePreviewPage = lazy(() =>
  import("@/pages/MobilePreviewPage").then((module) => ({ default: module.MobilePreviewPage }))
)
const ProfilesPage = lazy(() =>
  import("@/pages/ProfilesPage").then((module) => ({ default: module.ProfilesPage }))
)
const SettingsPage = lazy(() =>
  import("@/pages/SettingsPage").then((module) => ({ default: module.SettingsPage }))
)
const BudgetsPage = lazy(() =>
  import("@/pages/BudgetsPage").then((module) => ({ default: module.BudgetsPage }))
)
const SubagentsPage = lazy(() =>
  import("@/pages/SubagentsPage").then((module) => ({ default: module.SubagentsPage }))
)
const TasksPage = lazy(() =>
  import("@/pages/TasksPage").then((module) => ({ default: module.TasksPage }))
)
const WikiPage = lazy(() =>
  import("@/pages/WikiPage").then((module) => ({ default: module.WikiPage }))
)

function App() {
  return (
    <Suspense fallback={<PageFallback />}>
      <Routes>
        {/* Dev-only mobile UI comparison — outside AppLayout so the desktop
            shell never interferes with the phone frames. */}
        <Route path="/mobile-preview" element={<MobilePreviewPage />} />
        <Route element={<AppLayout />}>
          <Route index element={<ChatPage />} />
          <Route path="/chat/:conversationId" element={<ChatPage />} />
          <Route path="/memory" element={<MemoryPage />} />
          <Route path="/wiki" element={<WikiPage />} />
          <Route path="/profiles" element={<ProfilesPage />} />
          <Route path="/analytics" element={<AnalyticsPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/budgets" element={<BudgetsPage />} />
          <Route path="/subagents" element={<SubagentsPage />} />
          <Route path="/deep-research" element={<DeepResearchPage />} />
          <Route path="/tasks" element={<TasksPage />} />
          <Route path="/inspector" element={<InspectorPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
    </Suspense>
  )
}

function PageFallback() {
  return (
    <div
      className="grid h-dvh place-items-center bg-background text-sm text-muted-foreground"
      role="status"
      aria-live="polite"
    >
      Opening section…
    </div>
  )
}

export default App
