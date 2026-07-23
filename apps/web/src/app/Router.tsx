import {
  createBrowserRouter,
  Navigate,
  Outlet,
  RouterProvider,
  useLocation,
  useParams,
} from "react-router-dom";
import { useAuthStatus } from "../features/auth/hooks/useAuthStatus";
import { LoginPage } from "../features/auth/LoginPage";
import { SetupCredentialsPage } from "../features/auth/SetupCredentialsPage";
import { HomePage } from "../features/home/HomePage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { WorkspaceShell } from "../features/workspace/WorkspaceShell";
import { AppShell } from "./AppShell";

/**
 * Project workspace page with three-column layout
 */
function ProjectWorkspacePage() {
  const { projectId } = useParams<{ projectId: string }>();

  if (!projectId) {
    return (
      <div className="flex h-screen items-center justify-center">
        <div className="text-center">
          <h1 className="text-lg font-semibold text-foreground">
            Project not found
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            The requested project could not be found.
          </p>
        </div>
      </div>
    );
  }

  return <WorkspaceShell projectId={projectId} />;
}

/**
 * Root layout — wraps all routes in AppShell
 */
function RootLayout() {
  return (
    <AppShell>
      <Outlet />
    </AppShell>
  );
}

function ProtectedRoute({ children }: { readonly children: React.ReactNode }) {
  const location = useLocation();
  const authStatus = useAuthStatus();

  if (authStatus.isLoading) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Loading...
      </div>
    );
  }

  if (!authStatus.data?.authenticated) {
    return <Navigate to="/login" replace state={{ from: location.pathname }} />;
  }

  if (authStatus.data.user?.requiresCredentialSetup) {
    return <Navigate to="/setup" replace />;
  }

  return children;
}

const router = createBrowserRouter([
  {
    element: <RootLayout />,
    children: [
      {
        path: "/",
        element: (
          <ProtectedRoute>
            <HomePage />
          </ProtectedRoute>
        ),
      },
      {
        path: "/login",
        element: <LoginPage />,
      },
      {
        path: "/setup",
        element: <SetupCredentialsPage />,
      },
      {
        path: "/settings",
        element: (
          <ProtectedRoute>
            <SettingsPage />
          </ProtectedRoute>
        ),
      },
      {
        path: "/projects/:projectId",
        element: (
          <ProtectedRoute>
            <ProjectWorkspacePage />
          </ProtectedRoute>
        ),
      },
    ],
  },
]);

export function Router() {
  return <RouterProvider router={router} />;
}
