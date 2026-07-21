import { Outlet } from "react-router-dom"
import { Sidebar } from "./Sidebar"

/** Two-column app shell: sidebar (conversations, nav) + main content area. */
export function AppLayout() {
  return (
    <div className="flex h-screen w-screen overflow-hidden bg-background text-foreground">
      <Sidebar />
      <main className="flex-1 overflow-hidden">
        <Outlet />
      </main>
    </div>
  )
}
