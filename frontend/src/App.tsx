import { Routes, Route, Navigate } from "react-router-dom"
import { AppLayout } from "@/components/layout/AppLayout"
import { ChatPage } from "@/pages/ChatPage"
import { SettingsPage } from "@/pages/SettingsPage"

function App() {
  return (
    <Routes>
      <Route element={<AppLayout />}>
        <Route index element={<ChatPage />} />
        <Route path="/chat/:conversationId" element={<ChatPage />} />
        <Route path="/settings" element={<SettingsPage />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  )
}

export default App
