import { Routes, Route, Navigate } from "react-router-dom"
import { AppLayout } from "@/components/layout/AppLayout"
import { AnalyticsPage } from "@/pages/AnalyticsPage"
import { ChatPage } from "@/pages/ChatPage"
import { InspectorPage } from "@/pages/InspectorPage"
import { MemoryPage } from "@/pages/MemoryPage"
import { ProfilesPage } from "@/pages/ProfilesPage"
import { SettingsPage } from "@/pages/SettingsPage"
import { BudgetsPage } from "@/pages/BudgetsPage"
import { SubagentsPage } from "@/pages/SubagentsPage"
import { TasksPage } from "@/pages/TasksPage"
import { WikiPage } from "@/pages/WikiPage"

function App() {
  return (
    <Routes>
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
        <Route path="/tasks" element={<TasksPage />} />
        <Route path="/inspector" element={<InspectorPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}

export default App
