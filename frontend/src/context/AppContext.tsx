// frontend/src/context/AppContext.tsx
import React, {
  createContext,
  useContext,
  useState,
  useEffect,
  ReactNode,
} from "react";
import { getVersion } from "../api";

interface AppContextType {
  version: string;
  isAuthenticated: boolean;
  setIsAuthenticated: (status: boolean) => void;
  activeTab: string;
  setActiveTab: (tab: string) => void;
}

const defaultContext: AppContextType = {
  version: "",
  isAuthenticated: false,
  setIsAuthenticated: () => {},
  activeTab: "cookie",
  setActiveTab: () => {},
};

const AppContext = createContext<AppContextType>(defaultContext);

interface AppProviderProps {
  children: ReactNode;
}

export const AppProvider: React.FC<AppProviderProps> = ({ children }) => {
  const [version, setVersion] = useState("");
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const [activeTab, setActiveTab] = useState("cookie");

  // Utility function to strip ANSI escape sequences
  const stripAnsiCodes = (text: string): string => {
    return text.replace(/\x1b\[[0-9;]*m/g, '');
  };

  useEffect(() => {
    // Fetch and set the version when component mounts
    getVersion().then((v) => setVersion(stripAnsiCodes(v)));

    // Check for authentication status
    const checkAuth = async () => {
      const storedToken = localStorage.getItem("authToken");
      if (storedToken) {
        try {
          const response = await fetch("/api/auth", {
            method: "GET",
            headers: {
              Authorization: `Bearer ${storedToken}`,
              "Content-Type": "application/json",
            },
          });

          if (response.ok) {
            setIsAuthenticated(true);
          } else {
            // Invalid token, clear it
            localStorage.removeItem("authToken");
            setIsAuthenticated(false);
          }
        } catch (error) {
          console.error("Authentication check failed:", error);
          setIsAuthenticated(false);
        }
      }
    };

    checkAuth();
  }, []);

  return (
    <AppContext.Provider
      value={{
        version,
        isAuthenticated,
        setIsAuthenticated,
        activeTab,
        setActiveTab,
      }}
    >
      {children}
    </AppContext.Provider>
  );
};

export const useAppContext = () => useContext(AppContext);
