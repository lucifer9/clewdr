export interface ConfigData {
  // Server settings
  ip: string;
  port: number;

  // Vertex settings
  vertex: VertexConfig;

  // App settings
  check_update: boolean;
  auto_update: boolean;

  // Network settings
  password: string;
  admin_password: string;
  proxy: string | null;
  rproxy: string | null;

  // API settings
  max_retries: number;
  preserve_chats: boolean;
  web_search: boolean;

  // Fake streaming settings
  fake_streaming: boolean;
  fake_streaming_interval: number;

  // Cookie settings
  skip_first_warning: boolean;
  skip_second_warning: boolean;
  skip_restricted: boolean;
  skip_non_pro: boolean;
  skip_rate_limit: boolean;
  skip_normal_pro: boolean;

  // Prompt configurations
  use_real_roles: boolean;
  custom_h: string | null;
  custom_a: string | null;
  custom_prompt: string;

  // Tag validation settings
  required_tags: string;
  save_response_before_tag_check: boolean;
}

interface VertexConfig {
  credential: string | null;
  model_id: string | null;
}

export interface ConfigState {
  config: ConfigData | null;
  originalPassword: string;
  originalAdminPassword: string;
  loading: boolean;
  saving: boolean;
  error: string;
}
