// frontend/src/components/keys/KeyVisualization.tsx
import React, { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { getKeyStatus, deleteKey } from "../../api/keyApi";
import { KeyStatusInfo } from "../../types/key.types";
import Button from "../common/Button";
import LoadingSpinner from "../common/LoadingSpinner";
import StatusMessage from "../common/StatusMessage";
import KeyValue from "./KeyValue";
import DeleteButton from "./DeleteButton";

// Default empty state
const emptyKeyStatus: KeyStatusInfo = {
  valid: [],
};

const KeyVisualization: React.FC = () => {
  const { t } = useTranslation();
  const [keyStatus, setKeyStatus] = useState<KeyStatusInfo>(emptyKeyStatus);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refreshCounter, setRefreshCounter] = useState(0);
  const [deletingKey, setDeletingKey] = useState<string | null>(null);

  // Fetch key data
  const fetchKeyStatus = async () => {
    setLoading(true);
    setError(null);

    try {
      const data = await getKeyStatus();
      const safeData: KeyStatusInfo = {
        valid: Array.isArray(data?.valid) ? data.valid : [],
      };
      setKeyStatus(safeData);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setKeyStatus(emptyKeyStatus);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchKeyStatus();
  }, [refreshCounter]);

  const handleRefresh = () => setRefreshCounter((prev) => prev + 1);

  const handleDeleteKey = async (key: string) => {
    if (!window.confirm(t("keyStatus.deleteConfirm"))) return;

    setDeletingKey(key);
    setError(null);

    try {
      const response = await deleteKey(key);

      if (response.ok) {
        handleRefresh();
      } else {
        const errorMessage =
          response.status === 401
            ? t("keySubmit.error.auth")
            : await response
                .json()
                .then(
                  (data) =>
                    data.error ||
                    t("common.error", { message: response.status })
                );
        setError(errorMessage);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setDeletingKey(null);
    }
  };

  // Calculate total key count
  const totalKeys = keyStatus.valid.length;

  // Filter keys with cooldown and sort by cooldown time remaining
  const keysWithErrors = keyStatus.valid.filter(status => {
    const hasValidCooldown = status.cooldown_until && Date.now() / 1000 < status.cooldown_until;
    return hasValidCooldown;
  });

  const sortedKeys = keysWithErrors
    .slice()
    .sort((a, b) => {
      // Sort by remaining cooldown time (shortest first)
      const aRemaining = a.cooldown_until ? a.cooldown_until - Date.now() / 1000 : 0;
      const bRemaining = b.cooldown_until ? b.cooldown_until - Date.now() / 1000 : 0;
      return aRemaining - bRemaining;
    });

  return (
    <div className="space-y-6 w-full">
      {/* Header */}
      <div className="flex justify-between items-center mb-4 w-full">
        <div>
          <h3 className="text-lg font-semibold text-white">
            {t("keyStatus.title")}
          </h3>
          <p className="text-xs text-gray-400 mt-1">
            {t("keyStatus.total", { count: totalKeys })}
            {keysWithErrors.length > 0 && (
              <span className="text-yellow-400 ml-2">
                • {keysWithErrors.length} 个冷却中
              </span>
            )}
          </p>
        </div>
        <Button
          onClick={handleRefresh}
          className="p-2 bg-gray-700 hover:bg-gray-600 rounded-md transition-colors text-sm"
          disabled={loading}
          variant="secondary"
        >
          {loading ? (
            <span className="flex items-center">
              <svg
                className="animate-spin h-4 w-4 mr-2"
                fill="none"
                viewBox="0 0 24 24"
              >
                <circle
                  className="opacity-25"
                  cx="12"
                  cy="12"
                  r="10"
                  stroke="currentColor"
                  strokeWidth="4"
                ></circle>
                <path
                  className="opacity-75"
                  fill="currentColor"
                  d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                ></path>
              </svg>
              {t("keyStatus.refreshing")}
            </span>
          ) : (
            <span className="flex items-center">
              <svg
                xmlns="http://www.w3.org/2000/svg"
                className="h-4 w-4 mr-2"
                fill="none"
                viewBox="0 0 24 24"
                stroke="currentColor"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
                />
              </svg>
              {t("keyStatus.refresh")}
            </span>
          )}
        </Button>
      </div>

      {/* Error Display */}
      {error && <StatusMessage type="error" message={error} />}

      {/* Loading State */}
      {loading && totalKeys === 0 && (
        <div className="flex justify-center py-8">
          <LoadingSpinner size="lg" color="text-cyan-500" />
        </div>
      )}

      {/* Cooling Keys Section */}
      <div className="rounded-lg border border-yellow-800 bg-yellow-900/20 p-4">
        <h4 className="text-yellow-300 font-medium mb-3">
          冷却中的密钥
        </h4>

        {keysWithErrors.length === 0 ? (
          <div className="flex items-center text-green-400 text-sm py-4">
            <svg className="w-4 h-4 mr-2" fill="currentColor" viewBox="0 0 20 20">
              <path fillRule="evenodd" d="M10 18a8 8 0 100-16 8 8 0 000 16zm3.707-9.293a1 1 0 00-1.414-1.414L9 10.586 7.707 9.293a1 1 0 00-1.414 1.414l2 2a1 1 0 001.414 0l4-4z" clipRule="evenodd" />
            </svg>
            🎉 所有 {totalKeys} 个密钥状态正常
          </div>
        ) : (
          <div className="space-y-2">
            {sortedKeys.map((status) => (
              <div
                key={status.key}
                className="py-2 text-sm text-gray-300 flex flex-wrap justify-between items-start border-b border-yellow-800/30 last:border-0"
              >
                <div className="text-purple-300 flex-grow mr-4 min-w-0 mb-1 sm:mb-0">
                  <KeyValue keyString={status.key} />
                </div>
                <div className="flex items-center space-x-2">
                  {/* 429冷却显示 - 显示剩余冷却秒数 */}
                  {status.cooldown_until && Date.now() / 1000 < status.cooldown_until && (
                    <span className="text-yellow-400 bg-yellow-900/30 px-2 py-0.5 rounded text-xs">
                      429: {Math.ceil(status.cooldown_until - Date.now() / 1000)}s
                    </span>
                  )}
                  
                  <DeleteButton
                    keyString={status.key}
                    onDelete={handleDeleteKey}
                    isDeleting={deletingKey === status.key}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* No Keys Help Text */}
      {!loading && totalKeys === 0 && (
        <div className="mt-4 px-4 py-3 bg-gray-800/50 border border-gray-700 rounded-md">
          <p className="text-sm text-gray-300">{t("keyStatus.emptyHelp")}</p>
        </div>
      )}

      {/* Statistics Panel */}
      {totalKeys > 0 && (
        <div className="mt-4 px-4 py-3 bg-gray-800/50 border border-gray-700 rounded-md">
          <h5 className="text-sm font-medium text-gray-300 mb-2">密钥健康统计</h5>
          <div className="grid grid-cols-3 gap-3 text-sm">
            <div className="text-center">
              <div className="text-white text-lg font-bold">{totalKeys}</div>
              <div className="text-gray-400">总密钥</div>
            </div>
            <div className="text-center">
              <div className="text-green-400 text-lg font-bold">{totalKeys - keysWithErrors.length}</div>
              <div className="text-gray-400">健康</div>
            </div>
            <div className="text-center">
              <div className="text-yellow-400 text-lg font-bold">
                {keysWithErrors.length}
              </div>
              <div className="text-gray-400">冷却中</div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

export default KeyVisualization;
