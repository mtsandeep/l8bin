import { Navigate, Route, Routes, useNavigate } from 'react-router-dom';
import { AuthProvider, useAuth } from './components/AuthContext';
import ConnectPage from './components/ConnectPage';
import Footer from './components/Footer';
import HomePage from './components/HomePage';
import LoginScreen from './components/LoginScreen';
import NodesPage from './components/NodesPage';
import ScanImportPage from './components/ScanImportPage';
import GithubCallbackPage from './components/settings/GithubCallbackPage';
import SettingsPage from './components/settings/SettingsPage';
import { ToastProvider } from './components/ToastContext';

function AppContent() {
  const navigate = useNavigate();
  const { user, loading: authLoading } = useAuth();

  if (authLoading) {
    return (
      <div className="min-h-screen bg-slate-950 flex items-center justify-center">
        <div className="w-6 h-6 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
      </div>
    );
  }

  if (!user) {
    return <LoginScreen />;
  }

  return (
    <>
      <Routes>
        <Route path="/manage" element={<Navigate to="/" replace />} />
        <Route path="/manage/nodes" element={<NodesPage onBack={() => navigate('/')} />} />
        <Route
          path="/manage/import"
          element={
            <ScanImportPage
              onBack={() => navigate('/')}
              onDone={() => navigate('/', { state: { refresh: Date.now() } })}
            />
          }
        />
        <Route path="/settings" element={<Navigate to="/settings/general" replace />} />
        <Route path="/settings/general" element={<SettingsPage section="general" key="general" />} />
        <Route path="/settings/tokens" element={<SettingsPage section="tokens" key="tokens" />} />
        <Route path="/settings/backup" element={<SettingsPage section="backup" key="backup" />} />
        <Route path="/settings/github" element={<SettingsPage section="github" key="github" />} />
        <Route path="/settings/github/callback" element={<GithubCallbackPage key="github-callback" />} />
        <Route path="/connect" element={<ConnectPage />} />
        <Route path="/" element={<HomePage />} />
      </Routes>
      <Footer />
    </>
  );
}

function App() {
  return (
    <AuthProvider>
      <ToastProvider>
        <AppContent />
      </ToastProvider>
    </AuthProvider>
  );
}

export default App;
