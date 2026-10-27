// Generated from docs/gradient-api.yaml by gradient-api-spec. Do not edit.

export type ActionConfig = {
  recipients: string[];
  subject_template?: string | null;
  type: "send_mail";
} | {
  url: string;
  token?: string | null;
  type: "send_web_request";
} | {
  integration_id: IntegrationId;
  type: "git_host_status_report";
} | {
  integration_id: IntegrationId;
  generator?: PatchGeneratorKind;
  granularity?: PrGranularity;
  verify_gate?: VerifyGate;
  branch_pattern?: string;
  title_template?: string | null;
  body_template?: string | null;
  update_existing?: boolean;
  type: "open_pr";
} | {
  homeserver: string;
  room_id: string;
  access_token?: string | null;
  type: "send_matrix_message";
} | {
  webhook_url?: string | null;
  type: "send_slack_message";
};

export interface ActionResponse {
  id: TaskActionId;
  name: string;
  action_type: string;
  config: Value;
  events: string[];
  active: boolean;
  last_fired_at?: string | null;
  created_by: UserId;
  created_at: string;
  updated_at: string;
}

export interface Activity {
  days: ActivityDay[];
}

export interface ActivityDay {
  date: string;
  evaluations: number;
  failed: number;
}

export interface AddMemberRequest {
  user: string;
  role: string;
}

export type AddUpstreamRequest = {
  cache_name: string;
  display_name?: string | null;
  mode?: CacheSubscriptionMode | null;
  type: "internal";
} | {
  display_name: string;
  url: string;
  public_key: string;
  http1_only?: boolean;
  type: "http";
} | {
  url: string;
  remote_cache: string;
  display_name: string;
  mode?: CacheSubscriptionMode | null;
  api_key?: string | null;
  type: "gradient_proto";
};

export interface AdminTaskDto {
  id: AdminTaskId;
  kind: string;
  status: string;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  progress?: unknown;
  checkpoint?: string | null;
  error?: string | null;
  created_by?: UserId | null;
}

export type AdminTaskId = string;

export type ApiId = string;

export interface ApiKeyInfo {
  id: string;
  name: string;
  managed: boolean;
  permissions: string[];
  project?: string | null;
  created_at: string;
  last_used_at?: string | null;
  expires_at?: string | null;
  revoked_at?: string | null;
  allowed_ips: string[];
}

export interface ApiKeyPermissionsResponse {
  available_permissions: PermissionEntry[];
  availableCache: PermissionEntry[];
}

export interface ApiKeyRequest {
  name: string;
}

export interface ArtefactTree {
  evaluation: EvaluationId;
  created_at: string;
  entry_points: EntryPointArtefacts[];
}

export interface AssignDecisionView {
  at: string;
  worker_id: string;
  worker_name?: string | null;
  kind: number;
  winner?: string | null;
  candidates: DecisionCandidateView[];
}

export type AttemptEndReason = "completed" | "failed" | "timeout" | "lost" | "aborted";

export interface AttemptSummary {
  dispatched_job_id?: string | null;
  substitute: boolean;
  outcome: number;
  reason?: number | null;
  failure_message?: string | null;
  created_at: string;
}

export interface AuditLogEntry {
  id: string;
  event: string;
  ip?: string | null;
  user_agent?: string | null;
  metadata?: unknown;
  created_at: string;
}

export type BadgeStyle = "flat" | "flat-square";

export type Binary = string;

export interface BlobsResponse {
  uploaded: number;
  remaining: number;
}

export interface BoardCacheStats {
  totals: CacheTotals;
  traffic: SeriesPoint[];
  storage: SeriesPoint[];
}

export interface BoardExec {
  labels: string[];
  devices: DeviceClass[];
}

export interface BoardFleetPoint {
  bucket_start: string;
  connected: number;
  draining: number;
  eval: number;
  network: number;
  build: number;
}

export interface BoardHealth {
  version: string;
  uptime_seconds: number;
  workers_connected: number;
  jobs_pending: number;
  jobs_active: number;
  cache_bytes: number;
  cache_packages: number;
  process: ProcessStat;
  http: HttpRouteStat[];
  rollup_lag_seconds?: number | null;
  latest_rollup_bucket?: string | null;
  draining: boolean;
  supervised: SupervisedLoop[];
  proto_sessions: number;
  unconfirmed_nars: number;
  pending_deliveries: number;
  failed_deliveries: number;
  hot_nar_cache: HotNarCacheHealth;
}

export interface BoardNetworkStats {
  nar_egress: SeriesPoint[];
  workers: WorkerNet[];
  http: HttpRouteStat[];
}

export interface BoardProject {
  id: string;
  name: string;
  display_name: string;
}

export interface BoardStorage {
  granularity: string;
  op_latency: LabelledSeries[];
  op_errors: LabelledSeries[];
  lane_fill: LabelledSeries[];
  send_stalls: LabelledSeries[];
  serve_queue: LabelledSeries[];
  serve_failures: LabelledSeries[];
}

export interface BoardUpstreamCache {
  upstream_id: string;
  display_name: string;
  url: string;
  avg_latency_ms?: number | null;
  hit_rate?: number | null;
  requests_total: number;
  latency: SeriesPoint[];
  hit_rate_series: SeriesPoint[];
}

export interface BoardUpstreamCacheStats {
  upstream_caches: BoardUpstreamCache[];
}

export interface BoardWorker {
  id?: string | null;
  name?: string | null;
  projects: BoardProject[];
  draining: boolean;
  assigned_jobs: number;
  max_concurrent_builds: number;
  eval: boolean;
  network: boolean;
  build: boolean;
  architectures: string[];
  cpu_usage_pct?: number | null;
  ram_free_mb?: number | null;
  ram_total_mb?: number | null;
  exec?: BoardExec | null;
}

export interface BuildGraph {
  root: DerivationId;
  nodes: DependencyGraphNode[];
  edges: DependencyEdge[];
}

export interface BuildItem {
  id: BuildJobId;
  name: string;
  status: string;
  has_artefacts: boolean;
  updated_at: string;
  build_time_ms?: number | null;
  build_started_at?: string | null;
  dispatched_job?: DispatchedJobId | null;
  depth: number;
  prioritized: boolean;
  ifd: boolean;
}

export type BuildJobId = string;

export interface BuildProduct {
  file_type: string;
  subtype: string;
  name: string;
  path: string;
  size?: number | null;
}

export type BuildProductId = string;

export interface BuildProgress {
  phase: BuildProgressPhase;
  bytes_done: number;
  bytes_total?: number | null;
  paths_done: number;
  paths_total?: number | null;
}

export type BuildProgressPhase = "download" | "prefetch" | "upload";

export type BuildStatus = "Created" | "Queued" | "Building" | "Completed" | "FailedPermanent" | "Aborted" | "DependencyFailed" | "Substituted" | "FailedTransient" | "FailedTimeout" | "Skipped";

export interface BuildStatusCounts {
  completed: number;
  failed: number;
  building: number;
  queued: number;
  substituted: number;
  aborted: number;
}

export interface BuildWithOutputs {
  id: BuildJobId;
  evaluation: EvaluationId;
  status: BuildStatus;
  derivation_path: string;
  architecture: string;
  worker?: string | null;
  dispatched_job?: DispatchedJobId | null;
  output: { [key: string]: string };
  prioritized: boolean;
  ifd: boolean;
  created_at: string;
  updated_at: string;
  progress?: BuildProgress | null;
}

export interface Cache {
  id: CacheId;
  name: string;
  display_name: string;
  description: string;
  active: boolean;
  priority: number;
  local_priority?: number | null;
  public_key: string;
  public: boolean;
  created_by: UserId;
  created_at: string;
  managed: boolean;
  max_storage_gb: number;
  pull_through: boolean;
}

export type CacheId = string;

export interface CacheMetricPoint {
  time: string;
  bytes: number;
  requests: number;
}

export interface CacheResponse {
  id: CacheId;
  name: string;
  display_name: string;
  description: string;
  active: boolean;
  priority: number;
  local_priority?: number | null;
  max_storage_gb: number;
  pull_through: boolean;
  public_key: string;
  public: boolean;
  created_by: UserId;
  created_at: string;
  managed: boolean;
  can_edit: boolean;
}

export interface CacheRoleOwner {
  cache?: CacheId | null;
}

export interface CacheStatsResponse {
  total_bytes: number;
  total_nar_bytes: number;
  total_packages: number;
  storage_minutes: StorageMetricPoint[];
  storage_hours: StorageMetricPoint[];
  storage_days: StorageMetricPoint[];
  storage_weeks: StorageMetricPoint[];
  minutes: CacheMetricPoint[];
  hours: CacheMetricPoint[];
  days: CacheMetricPoint[];
  weeks: CacheMetricPoint[];
}

export interface CacheSubscriptionItem {
  id: CacheId;
  name: string;
  mode: CacheSubscriptionMode;
  status: SubscriptionStatus;
}

export type CacheSubscriptionMode = "ReadWrite" | "ReadOnly" | "WriteOnly";

export interface CacheTotals {
  bytes: number;
  nar_bytes: number;
  packages: number;
  bytes_sent_total: number;
  requests_total: number;
}

export type CacheUpstreamId = string;

export interface CatalogEntry {
  name: string;
  durable: boolean;
}

export interface CheckRepositoryRequest {
  repository?: string | null;
}

export interface CheckUsernameRequest {
  username: string;
}

export interface ChunkReceived {
  received: number;
}

export interface CliDeviceAuthorizeRequest {
  user_code: string;
}

export interface CliDeviceInfoResponse {
  user_code: string;
  expires_at: string;
  user_agent?: string | null;
  ip?: string | null;
}

export interface CliDevicePollRequest {
  device_code: string;
}

export interface CliDeviceStartResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string;
  expires_in: number;
  interval: number;
}

export interface ClosureEdge {
  source: string;
  target: string;
}

export interface ClosureGraph {
  roots: string[];
  total_size_bytes?: number | null;
  node_count: number;
  edge_count: number;
  truncated: boolean;
  nodes: ClosureNode[];
  edges: ClosureEdge[];
}

export interface ClosureNode {
  id: string;
  name: string;
  path: string;
  nar_size?: number | null;
}

export interface Commit {
  id: CommitId;
  message: string;
  hash: number[];
  author?: UserId | null;
  author_name: string;
}

export type CommitId = string;

export interface ComparedEvaluation {
  id: EvaluationId;
  commit: string;
}

export type ConcurrencyPolicy = "hard_abort" | "soft_abort" | "all" | "skip" | "auto";

export type ConnectionDirection = "outbound" | "inbound";

export interface ConnectionFailure {
  reason: string;
  at: string;
  direction: ConnectionDirection;
  before_auth: boolean;
}

export interface ConnectionRequest {
  scope: ConnectionScope;
  team?: string | null;
  project?: string | null;
  token: string;
}

export interface ConnectionResponse {
  worker_id: string;
}

export type ConnectionScope = "team" | "project";

export interface Counts {
  all: number;
  failing: number;
  starred: number;
}

export interface CreateActionRequest {
  name: string;
  config: ActionConfig;
  events?: string[];
  active?: boolean;
}

export interface CreateActionResponse {
  action: ActionResponse;
  token?: string | null;
}

export interface CreateApiKeyRequest {
  name: string;
  expires_in_days?: number | null;
  permissions: string[];
  project?: string | null;
  cache?: string | null;
  allowed_ips?: string[] | null;
}

export interface CreateFlakeInputOverrideRequest {
  input_name: string;
  url?: string | null;
}

export interface CreateIntegrationRequest {
  name: IndexName;
  display_name?: string | null;
  kind: string;
  git_host_type: string;
  secret?: string | null;
  endpoint_url?: string | null;
  access_token?: string | null;
  allowed_ips?: string[] | null;
  installation_id?: number | null;
}

export type CreatePermission = "none" | "superusers" | "everyone";

export interface CreateRoleRequest {
  name: IndexName;
  permissions: string[];
}

export interface CreateSshKeyRequest {
  name: string;
  public_key: string;
}

export interface CreateTriggerRequest {
  config: TriggerConfig;
  active?: boolean;
}

export interface CreateWebhookRequest {
  name: string;
  url: string;
  events?: string[];
  active?: boolean;
}

export interface CreateWebhookResponse {
  webhook: WebhookResponse;
  secret: string;
}

export interface DashboardStats {
  cpu_time_ms: number;
  cpu_time_ms_7d: number;
  builds_completed: number;
  cache_size_bytes: number;
  workers: OnlineWorkerLoad;
  queue_wait_p50_ms: number;
}

export interface DebugInfoRedirect {
  archive: string;
  member: string;
}

export interface DecisionCandidateView {
  id: string;
  job_id: string;
  kind: number;
  project: string;
  build_id?: string | null;
  evaluation_id: string;
  subject?: string | null;
  score: number;
  won: boolean;
}

export interface DeleteUserRequest {
  password?: string | null;
  confirm_username?: string | null;
}

export interface DeletedResponse {
  deleted: boolean;
}

export type DeliveryDetail_TaskActionDeliveryId = DeliveryListItem_TaskActionDeliveryId & {
  request_body: string;
  response_body?: string | null;
};

export type DeliveryDetail_WebhookDeliveryId = DeliveryListItem_WebhookDeliveryId & {
  request_body: string;
  response_body?: string | null;
};

export interface DeliveryListItem {
  id: TaskActionDeliveryId;
  event: string;
  success: boolean;
  response_status?: number | null;
  error_message?: string | null;
  duration_ms: number;
  delivered_at: string;
}

export interface DeliveryListItem_TaskActionDeliveryId {
  id: string;
  event: string;
  success: boolean;
  response_status?: number | null;
  error_message?: string | null;
  duration_ms: number;
  delivered_at: string;
}

export interface DeliveryListItem_WebhookDeliveryId {
  id: string;
  event: string;
  success: boolean;
  response_status?: number | null;
  error_message?: string | null;
  duration_ms: number;
  delivered_at: string;
}

export interface DependencyEdge {
  source: DerivationId;
  target: DerivationId;
}

export interface DependencyGraphNode {
  id: DerivationId;
  build?: BuildJobId | null;
  name: string;
  path: string;
  status?: string | null;
  created_at: string;
  updated_at: string;
}

export type DerivationId = string;

export interface DeviceClass {
  name: string;
  total: number;
  free: number;
}

export interface DeviceRequest {
  name: string;
  count: number;
}

export interface DispatchRequest {
  target?: string | null;
  system?: string | null;
  input_overrides?: InputOverrideBody[];
}

export interface DispatchResponse {
  evaluation: EvaluationId;
  task: TaskId;
  commit: CommitId;
  cache?: string | null;
}

export type DispatchedJobDetail = ReportGap & {
  id: string;
  kind: number;
  project: string;
  project_name: string;
  project_display_name: string;
  worker_id: string;
  worker_name?: string | null;
  score: number;
  queued_at: string;
  dispatched_at: string;
  finished_at?: string | null;
  ready_at?: string | null;
  outcome?: string | null;
  phases: JobPhaseView[];
  build_id?: string | null;
  derivation_build_id?: string | null;
  derivations: JobDerivationView[];
  evaluation_id: string;
  evaluation?: JobEvaluationView | null;
  pname?: string | null;
  score_breakdown: unknown;
  worker_context: unknown;
  job_context: unknown;
  instance_context: unknown;
  candidates?: unknown;
  previous_attempts: AttemptSummary[];
  passed_over: boolean;
};

export type DispatchedJobId = string;

export interface DispatchedJobSummary {
  id: string;
  kind: number;
  project: string;
  worker_id: string;
  worker_name?: string | null;
  score: number;
  dispatched_at: string;
  build_id?: string | null;
  evaluation_id: string;
  subject?: string | null;
}

export interface DispatchedJobsResponse {
  jobs: DispatchedJobSummary[];
  other_running: number;
}

export type DisplayName = string;

export interface DrainingResponse {
  draining: boolean;
}

export interface DurationsHeatmap {
  times: string[];
  bands: HeatmapBand[];
}

export interface EmailResource {
  value: string;
  primary?: boolean;
}

export interface EntryPointArtefacts {
  attr: string;
  derivation: string;
  build_id: BuildJobId;
  outputs: OutputArtefacts[];
}

export interface EntryPointBrief {
  id: EntryPointId;
  eval: string;
  build_status: BuildStatus;
}

export type EntryPointId = string;

export interface EntryPointMetricPoint {
  evaluation_id: EvaluationId;
  build_id: BuildJobId;
  created_at: string;
  build_status: BuildStatus;
  build_time_ms?: number | null;
  output_size_bytes?: number | null;
  closure_size_bytes?: number | null;
  runtime_closure_size_bytes?: number | null;
  dependencies_count: number;
}

export interface EntryPointMetricsResponse {
  eval: string;
  keep_evaluations: number;
  points: EntryPointMetricPoint[];
}

export interface EntryPointSummary {
  id: EntryPointId;
  build_id: BuildJobId;
  derivation_path: string;
  eval: string;
  build_status: BuildStatus;
  has_artefacts: boolean;
  outputs: { [key: string]: string };
  architecture: string;
  build_time_ms?: number | null;
  build_started_at?: string | null;
  deps: BuildStatusCounts;
  deps_total: number;
  prioritized: boolean;
  ifd: boolean;
  created_at: string;
}

export interface ErrorResponseBody {
  error: boolean;
  code: string;
  message: string;
}

export type EvalCapability = "network" | "secrets" | "eval" | "exec";

export interface EvaluateRequest {
  commit?: string | null;
  attr?: string | null;
  walk?: WalkMode | null;
}

export interface EvaluationFailureSummary {
  compared_with?: ComparedEvaluation | null;
  packages: PackageCounts;
  failures: FailedBuildSummary[];
  failures_total: number;
  failed_attributes: FailedAttributeReport[];
  fixed: string[];
  fixed_total: number;
}

export type EvaluationId = string;

export type EvaluationMessageId = string;

export interface EvaluationMessageResponse {
  id: EvaluationMessageId;
  level: MessageLevel;
  message: string;
  source?: string | null;
  created_at: string;
  entry_points: EntryPointId[];
}

export type EvaluationProgress = {
  inputs: InputFetch[];
  kind: "fetching";
} | {
  thunks: number;
  kind: "evaluating";
};

export interface EvaluationResponse {
  id: EvaluationId;
  task?: TaskId | null;
  task_name?: string | null;
  task_display_name?: string | null;
  repository: string;
  commit: string;
  wildcard: string;
  status: EvaluationStatus;
  previous?: EvaluationId | null;
  next?: EvaluationId | null;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  updated_at: string;
  error_count: number;
  warning_count: number;
  error?: string | null;
  entry_points: EntryPointBrief[];
  prioritized: boolean;
  trigger?: EvaluationTriggerSummary | null;
  triggered_by?: string | null;
  waiting_reason?: WaitingReason | null;
  progress?: EvaluationProgress | null;
  expected_thunks?: number | null;
}

export type EvaluationStatus = "Queued" | "EvaluatingFlake" | "EvaluatingDerivation" | "Building" | "Waiting" | "Completed" | "Failed" | "Aborted" | "Fetching";

export interface EvaluationSummary {
  id: EvaluationId;
  commit: string;
  commit_message?: string | null;
  status: EvaluationStatus;
  wildcard: string;
  trigger?: EvaluationTriggerSummary | null;
  triggered_by?: string | null;
  pr_number?: number | null;
  total_builds: number;
  builds: BuildStatusCounts;
  errors: number;
  warnings: number;
  dispatched_job?: DispatchedJobId | null;
  prioritized: boolean;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  updated_at: string;
  progress?: EvaluationProgress | null;
  expected_thunks?: number | null;
}

export interface EvaluationTriggerSummary {
  id: TaskTriggerId;
  type: TriggerType;
}

export interface ExecAllow {
  network: boolean;
  ssh: boolean;
  ports: never[];
  nodes: number;
  devices: string[];
  mounts: string[];
  custom_images: boolean;
  timeout_secs?: number | null;
  max_silent_secs?: number | null;
  retries: number;
}

export interface ExecDefaults {
  network: boolean;
  ssh: boolean;
  mounts: string[];
  max_silent_secs?: number | null;
}

export interface ExecInventory {
  labels: string[];
  devices: DeviceClass[];
  mounts: string[];
  secrets: string[];
  allow: ExecAllow;
  defaults: ExecDefaults;
  warm_guests: number;
}

export interface ExecJobAttemptView {
  number: number;
  worker: string;
  started_at: string;
  ended_at?: string | null;
  end_reason?: AttemptEndReason | null;
  message?: string | null;
}

export type ExecJobId = string;

export interface ExecJobPhaseView {
  name: string;
  attempt: number;
  created_at: string;
}

export interface ExecJobResponse {
  id: ExecJobId;
  status: ExecJobStatus;
  entry_point: string;
  evaluation: EvaluationId;
  task: string;
  project: string;
  nodes: number;
  labels: string[];
  devices: DeviceRequest[];
  waiting_reason?: WaitingReason | null;
  message?: string | null;
  retries_left: number;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
  worker?: string | null;
  attempts: ExecJobAttemptView[];
  phases: ExecJobPhaseView[];
}

export type ExecJobStatus = "queued" | "starting" | "running" | "completed" | "failed" | "aborted" | "created";

export interface ExecJobSummary {
  id: ExecJobId;
  entry_point: string;
  status: ExecJobStatus;
  waiting_reason?: WaitingReason | null;
  message?: string | null;
  nodes: number;
  labels: string[];
  devices: DeviceRequest[];
  started_at?: string | null;
  last_phase?: string | null;
}

export interface ExecUnmet {
  labels: string[];
  devices: DeviceRequest[];
  mounts: string[];
  nodes: number;
  custom_image: boolean;
}

export interface ExpensiveBuild {
  build_id: string;
  project: string;
  name: string;
  build_time_ms: number;
  worker?: string | null;
  worker_name?: string | null;
}

export interface ExpensiveEval {
  evaluation: string;
  project: string;
  project_name: string;
  project_display_name: string;
  task_name: string;
  task_display_name: string;
  name: string;
  value: number;
  unit: string;
  worker: string;
  worker_name?: string | null;
}

export interface ExpensiveResource {
  derivation: string;
  project: string;
  name: string;
  value: number;
  unit: string;
  worker: string;
  worker_name?: string | null;
}

export interface FailedAttributeReport {
  attr: string;
  message: string;
  newly_failing: boolean;
}

export interface FailedAttributeSummary {
  eval: string;
  message: string;
}

export interface FailedBuildSummary {
  build_id: BuildJobId;
  name: string;
  derivation_path: string;
  architecture: string;
  status: BuildStatus;
  attributes: string[];
  blocked: string[];
  blocked_total: number;
  newly_failing: boolean;
}

export type FileTree = {
  executable?: boolean;
  size: number;
  narOffset?: number | null;
  type: "regular";
} | {
  target: string;
  type: "symlink";
} | {
  entries: { [key: string]: FileTree };
  type: "directory";
};

export type Filter = "all" | "failing" | "starred";

export type FireOutcome = {
  evaluation_id: EvaluationId;
  outcome: "Created";
} | {
  outcome: "SkippedSameCommit";
} | {
  outcome: "SkippedConcurrency";
} | {
  outcome: "InputUpdateRunning";
} | {
  evaluation_ids: EvaluationId[];
  outcome: "InputUpdateStarted";
};

export interface FlakeGraphNode {
  path: string;
  parent?: string | null;
  name: string;
  kind: string;
  is_derivation: boolean;
  drv_path?: string | null;
}

export type FlakeInputOverrideId = string;

export interface FlakeInputOverrideOut {
  id: FlakeInputOverrideId;
  task: TaskId;
  input_name: string;
  url?: string | null;
  created_at: string;
  updated_at: string;
}

export interface GetUserSettingsResponse {
  username: string;
  name: string;
  email: string;
  is_oidc: boolean;
  managed: boolean;
}

export interface GithubAppManifestRequest {
  host?: string | null;
}

export interface GithubAppManifestResponse {
  manifest: unknown;
  post_url: string;
  state: string;
}

export interface GradientCacheInfo {
  GradientVersion: string;
  GradientUrl: string;
}

export interface GradientCapabilities {
  core: boolean;
  federate: boolean;
  eval: boolean;
  build: boolean;
  cache: boolean;
  network: boolean;
  secrets: boolean;
  exec: boolean;
}

export interface GrantCacheRequest {
  team: string;
  role: string;
}

export interface GrantProjectRequest {
  team: string;
  role?: string | null;
  users?: boolean;
  workers?: boolean;
}

export interface GroupMember {
  value: string;
  display?: string | null;
}

export interface GroupResource {
  schemas: string[];
  id: string;
  displayName: string;
  members: GroupMember[];
  meta: Meta;
}

export interface HeatmapBand {
  band: string;
  counts: number[];
}

export interface HistoryBar {
  id: EvaluationId;
  status: EvaluationStatus;
  duration_ms?: number | null;
  created_at: string;
}

export type HitKind = "project" | "task" | "cache" | "nar" | "commit";

export interface HotNarCacheHealth {
  entries: number;
  bytes: number;
  hits: number;
  misses: number;
  evictions: number;
}

export interface HttpRouteStat {
  method: string;
  route: string;
  count: number;
  avg_ms: number;
  errors: number;
}

export interface IncomingWebhookResponse {
  event: string;
  repository_urls: string[];
  tasks_scanned: number;
  queued: QueuedEvaluation[];
  skipped: SkippedTask[];
}

export type IndexName = string;

export interface InputFetch {
  name: string;
  state: InputFetchState;
  downloaded_bytes: number;
  expected_bytes: number;
}

export type InputFetchState = "Queued" | "Fetching" | "Done" | "Failed";

export interface InputOverrideBody {
  input_name: string;
  url: string;
}

export type IntegrationId = string;

export interface IntegrationResponse {
  id: IntegrationId;
  project: ProjectId;
  name: string;
  display_name: string;
  kind: string;
  git_host_type: string;
  endpoint_url?: string | null;
  has_secret: boolean;
  has_access_token: boolean;
  allowed_ips: string[];
  installation_id?: number | null;
  account_login?: string | null;
  created_by: UserId;
  created_at: string;
  managed: boolean;
}

export interface IntegrationSummaryResponse {
  id: IntegrationId;
  name: string;
  display_name: string;
  kind: string;
  git_host_type: string;
}

export interface InviteItem {
  kind: InviteKind;
  token: string;
  scope: string;
  scope_display_name: string;
  role: string;
  invited_by: string;
  created_at: string;
  expires_at: string;
}

export type InviteKind = "project" | "cache" | "team";

export interface InviteTokenRequest {
  token: string;
}

export interface JobDerivationView {
  build?: string | null;
  derivation_build: string;
  drv_path: string;
  pname?: string | null;
}

export interface JobEvaluationView {
  repository: string;
  commit: string;
  commit_message?: string | null;
  wildcard: string;
  task?: string | null;
}

export interface JobPhaseView {
  seq: number;
  parent_seq?: number | null;
  phase: string;
  start_ms: number;
  end_ms: number;
  paths: number;
  bytes: number;
}

export interface LabelledPoint {
  bucket_start: string;
  count: number;
  avg: number;
  max: number;
}

export interface LabelledSeries {
  label: string;
  points: LabelledPoint[];
}

export interface Latest {
  id: EvaluationId;
  status: EvaluationStatus;
  commit: string;
  created_at: string;
}

export interface ListResponse_GroupResource {
  schemas: string[];
  totalResults: number;
  startIndex: number;
  itemsPerPage: number;
  Resources: ({
    schemas: string[];
    id: string;
    displayName: string;
    members: GroupMember[];
    meta: Meta;
  })[];
}

export interface ListResponse_UserResource {
  schemas: string[];
  totalResults: number;
  startIndex: number;
  itemsPerPage: number;
  Resources: ({
    schemas: string[];
    id: string;
    userName: string;
    externalId?: string | null;
    name: NameResource;
    displayName: string;
    emails: EmailResource[];
    active: boolean;
    meta: Meta;
  })[];
}

export interface LoadBucket {
  key: string;
  in_flight: number;
  capacity: number;
  workers: number;
}

export interface LogChunkIndex {
  total_chunks: number;
  total_lines: number;
  total_bytes: number;
  chunks: LogChunkMeta[];
}

export interface LogChunkMeta {
  index: number;
  line_start: number;
  line_count: number;
  byte_start: number;
  byte_len: number;
}

export interface LogSearchHit {
  line_number: number;
  chunk_index: number;
  byte_offset: number;
  preview: string;
}

export interface MakeCacheRequest {
  name: IndexName;
  display_name: DisplayName;
  description: string;
  priority: number;
  public?: boolean | null;
  local_priority?: number | null;
  max_storage_gb?: number | null;
  pull_through?: boolean | null;
}

export interface MakeEvaluationRequest {
  method: string;
}

export interface MakeLoginRequest {
  loginname: string;
  password: string;
  remember_me?: boolean;
}

export interface MakeProjectRequest {
  name: IndexName;
  display_name: DisplayName;
  description: string;
  public?: boolean | null;
  hide_build_requests?: boolean | null;
}

export interface MakeTaskRequest {
  name: IndexName;
  display_name: DisplayName;
  description: string;
  repository: string;
  wildcard: string;
  concurrency?: ConcurrencyPolicy | null;
  sign_cache?: boolean | null;
  wait_for_workers?: boolean | null;
  retry_failed_builds?: boolean | null;
  multi_node_evaluation?: boolean | null;
}

export interface MakeTeamRequest {
  name: IndexName;
  display_name: DisplayName;
}

export interface MakeUserRequest {
  username: string;
  name: DisplayName;
  email: string;
  password: string;
}

export interface ManifestEntry {
  path: string;
  hash: string;
  size: number;
}

export interface ManifestRequest {
  project: string;
  files: ManifestEntry[];
}

export interface ManifestResponse {
  session: UploadSessionId;
  missing: string[];
}

export interface ManifestResult {
  id: number;
  slug: string;
  html_url: string;
  pem: string;
  webhook_secret: string;
  client_id: string;
  client_secret: string;
}

export interface MemberItem {
  id: string;
  name: string;
}

export type MessageLevel = "Error" | "Warning" | "Notice";

export interface Meta {
  resourceType: string;
}

export interface MetricMeta {
  key: string;
  kind: string;
  unit: string;
  dimensions: string[];
}

export interface MetricPoint {
  bucket_start: string;
  count: number;
  sum: number;
  min: number;
  max: number;
  avg: number;
}

export interface NameRequest {
  formatted?: string | null;
  givenName?: string | null;
  familyName?: string | null;
}

export interface NameResource {
  formatted: string;
}

export interface NarAvailable {
  available: boolean;
}

export interface NarDetail {
  hash: string;
  store_path: string;
  package: string;
  nar_size?: number | null;
  file_size?: number | null;
  file_hash?: string | null;
  nar_hash?: string | null;
  references: string[];
  deriver?: string | null;
  ca?: string | null;
  created_at: string;
  last_fetched_at?: string | null;
  fetch_count: number;
  signed: boolean;
}

export interface NarList {
  version: number;
  root: FileTree;
}

export interface NarStats {
  total_nars: number;
  total_nar_size: number;
  total_file_size: number;
  last_uploaded_at?: string | null;
  oldest_fetched_at?: string | null;
}

export interface NarSummary {
  hash: string;
  store_path: string;
  package: string;
  nar_size?: number | null;
  file_size?: number | null;
  created_at: string;
  last_fetched_at?: string | null;
}

export interface NarUploadForm {
  narinfo: NarinfoPart;
  nar: string;
}

export interface NarinfoPart {
  store_path: string;
  file_hash: string;
  file_size: number;
  nar_size: number;
  nar_hash: string;
  references?: string[];
  deriver?: string | null;
}

export interface NixCacheInfo {
  WantMassQuery: boolean;
  StoreDir: string;
  Priority: number;
}

export interface NixPathInfo {
  StorePath: string;
  URL: string;
  Compression: string;
  FileHash: string;
  FileSize: number;
  NarHash: string;
  NarSize: number;
  References: string[];
  Sig: string;
  Deriver?: string | null;
  CA?: string | null;
}

export interface NixRealisation {
  outPath: string;
  signatures: string[];
}

export interface OnlineWorkerLoad {
  online: number;
  busy_pct: number;
}

export interface Outcomes {
  ok: number;
  failing: number;
  total: number;
}

export interface OutputArtefacts {
  name: string;
  store_path: string;
  products: ProductArtefact[];
}

export interface PackageCounts {
  total: number;
  built: number;
  failed: number;
  blocked: number;
  unfinished: number;
}

export interface PaginatedBuilds {
  builds: BuildItem[];
  total: number;
  active_count: number;
}

export interface PaginatedEntryPoints {
  entry_points: EntryPointSummary[];
  total: number;
  failed_attributes: FailedAttributeSummary[];
}

export interface Paginated_AuditLogEntry {
  items: AuditLogEntry[];
  total: number;
  page: number;
  per_page: number;
}

export interface Paginated_Cache {
  items: Cache[];
  total: number;
  page: number;
  per_page: number;
}

export interface Paginated_NarSummary {
  items: NarSummary[];
  total: number;
  page: number;
  per_page: number;
}

export interface Paginated_Project {
  items: Project[];
  total: number;
  page: number;
  per_page: number;
}

export interface Paginated_ProjectSummary {
  items: ProjectSummary[];
  total: number;
  page: number;
  per_page: number;
}

export interface Paginated_TaskResponse {
  items: TaskResponse[];
  total: number;
  page: number;
  per_page: number;
}

export interface PatchApiKeyRequest {
  name?: string | null;
  permissions?: string[] | null;
  project?: string | null;
  allowed_ips?: string[] | null;
}

export interface PatchCacheGrantRequest {
  role: string;
}

export interface PatchCacheRequest {
  name?: IndexName | null;
  display_name?: string | null;
  description?: string | null;
  priority?: number | null;
  local_priority?: number | null;
  max_storage_gb?: number | null;
  pull_through?: boolean | null;
}

export type PatchGeneratorKind = "flake_lock";

export interface PatchIntegrationRequest {
  name?: IndexName | null;
  display_name?: string | null;
  git_host_type?: string | null;
  endpoint_url?: string | null;
  secret?: string | null;
  access_token?: string | null;
  allowed_ips?: string[] | null;
}

export interface PatchOperation {
  op: string;
  path?: string | null;
  value?: unknown;
}

export interface PatchProjectGrantRequest {
  role?: string | null;
  users?: boolean | null;
  workers?: boolean | null;
}

export interface PatchProjectRequest {
  name?: IndexName | null;
  display_name?: string | null;
  description?: string | null;
  hide_build_requests?: boolean | null;
  exec_enabled?: boolean | null;
}

export interface PatchRequest {
  Operations: PatchOperation[];
}

export interface PatchRoleRequest {
  name?: IndexName | null;
  permissions?: string[] | null;
}

export interface PatchTaskRequest {
  name?: IndexName | null;
  display_name?: string | null;
  description?: string | null;
  repository?: string | null;
  wildcard?: string | null;
  keep_evaluations?: number | null;
  concurrency?: ConcurrencyPolicy | null;
  sign_cache?: boolean | null;
  wait_for_workers?: boolean | null;
  retry_failed_builds?: boolean | null;
  multi_node_evaluation?: boolean | null;
}

export interface PatchTeamRequest {
  display_name?: DisplayName | null;
  new_project_users?: boolean | null;
  new_project_workers?: boolean | null;
  new_project_role?: string | null;
  oidc_group?: string | null;
  scim_group?: string | null;
}

export interface PatchUpstreamRequest {
  display_name?: string | null;
  mode?: CacheSubscriptionMode | null;
  url?: string | null;
  public_key?: string | null;
  http1_only?: boolean | null;
  active?: boolean | null;
}

export interface PatchUserSettingsRequest {
  username?: string | null;
  name?: DisplayName | null;
  email?: string | null;
}

export interface PatchWorkerRequest {
  active?: boolean | null;
  display_name?: string | null;
  enable_network?: boolean | null;
  enable_secrets?: boolean | null;
  enable_exec?: boolean | null;
  enable_eval?: boolean | null;
  enable_build?: boolean | null;
}

export interface PendingInvitationItem {
  user: string;
  name: string;
  role: string;
  created_at: string;
  expires_at: string;
}

export interface PendingJobSummary {
  kind: number;
  project: string;
  evaluation_id: string;
  build_id?: string | null;
  queued_at: string;
  dependency_count: number;
  subject?: string | null;
}

export interface PendingJobsResponse {
  jobs: PendingJobSummary[];
  other_pending: number;
}

export interface PermissionEntry {
  id: string;
  mutating: boolean;
}

export type PrGranularity = "per_run" | "per_input";

export interface ProcessStat {
  resident_memory_bytes: number;
  virtual_memory_bytes: number;
  open_fds: number;
  max_fds: number;
  cpu_seconds_total: number;
  threads: number;
}

export interface ProductArtefact {
  id: BuildProductId;
  type: string;
  subtype: string;
  name: string;
  path: string;
  size?: number | null;
}

export interface Project {
  id: ProjectId;
  name: string;
  display_name: string;
  description: string;
  public_key: string;
  private_key: string;
  public: boolean;
  hide_build_requests: boolean;
  exec_enabled: boolean;
  created_by: UserId;
  created_at: string;
  managed: boolean;
}

export type ProjectId = string;

export interface ProjectResponse {
  id: ProjectId;
  name: string;
  display_name: string;
  description: string;
  public_key?: string | null;
  public: boolean;
  hide_build_requests: boolean;
  exec_enabled: boolean;
  managed: boolean;
  created_by: UserId;
  created_at: string;
  github_app_available: boolean;
  github_app_install_url?: string | null;
  role?: string | null;
}

export interface ProjectRoleOwner {
  project?: ProjectId | null;
}

export interface ProjectSummary {
  id: ProjectId;
  name: string;
  display_name: string;
  description: string;
  public_key?: string | null;
  public: boolean;
  hide_build_requests: boolean;
  managed: boolean;
  created_by: UserId;
  created_at: string;
  running_evaluations: number;
  role?: string | null;
}

export type ProjectWorkerEntry = WorkerConnection & {
  worker_id: string;
  display_name: string;
  registered_at: string;
  active: boolean;
  url?: string | null;
  created_by?: UserId | null;
  enable_network: boolean;
  enable_secrets: boolean;
  enable_exec: boolean;
  enable_eval: boolean;
  enable_build: boolean;
  team?: string | null;
  gradient_ci: boolean;
  live?: WorkerLiveInfo | null;
};

export interface ProtocolProbe {
  ok: boolean;
  status?: number | null;
  latency_ms: number;
  error?: string | null;
}

export interface QueueSummary {
  building: number;
  queued: number;
}

export interface QueuedEvaluation {
  task_id: TaskId;
  task_name: string;
  project: string;
  evaluation_id: EvaluationId;
}

export interface Rail {
  projects: RailProject[];
  caches: RailCache[];
}

export interface RailCache {
  name: string;
  display_name: string;
  starred: boolean;
  nar_count: number;
}

export interface RailProject {
  name: string;
  display_name: string;
  starred: boolean;
  tier: Tier;
  status?: EvaluationStatus | null;
  task_count: number;
  tasks?: RailTask[] | null;
}

export interface RailTask {
  name: string;
  status?: EvaluationStatus | null;
}

export interface RegisterWorkerRequest {
  worker_id: string;
  url?: string | null;
  display_name: string;
  token?: string | null;
  enable_network?: boolean;
  enable_secrets?: boolean;
  enable_exec?: boolean;
  enable_eval?: boolean;
  enable_build?: boolean;
}

export interface RegisterWorkerResponse {
  peer_id: ProjectId;
  token?: string | null;
}

export interface RegisteredTeamWorker {
  team: TeamId;
  token?: string | null;
}

export interface RemoveMemberRequest {
  user: string;
}

export interface RemoveTeamMemberRequest {
  user: string;
}

export interface ReportGap {
  worker_elapsed_ms?: number | null;
  worker_tail_ms?: number | null;
  transit_ms?: number | null;
}

export type RequestKind = "project" | "cache";

export interface RevokeInvitationRequest {
  user: string;
}

export type RoleId = string;

export interface RoleListResponse_CacheRoleOwner {
  roles: RoleResponse_CacheRoleOwner[];
  available_permissions: PermissionEntry[];
}

export interface RoleListResponse_ProjectRoleOwner {
  roles: RoleResponse_ProjectRoleOwner[];
  available_permissions: PermissionEntry[];
}

export type RoleResponse_CacheRoleOwner = {
  cache?: CacheId | null;
} & {
  id: RoleId;
  name: string;
  builtin: boolean;
  managed: boolean;
  permissions: string[];
};

export type RoleResponse_ProjectRoleOwner = {
  project?: ProjectId | null;
} & {
  id: RoleId;
  name: string;
  builtin: boolean;
  managed: boolean;
  permissions: string[];
};

export interface RotatedSecret {
  secret: string;
}

export interface RuleContribution {
  rule: string;
  avg: number;
  min: number;
  max: number;
}

export interface RuleDescription {
  rule: string;
  description: string;
}

export interface ScimErrorBody {
  schemas: string[];
  status: string;
  scimType?: string | null;
  detail: string;
}

export interface ScoreBucket {
  lo: number;
  hi: number;
  count: number;
}

export interface ScoringSummary {
  sample_size: number;
  score_min: number;
  score_max: number;
  score_avg: number;
  histogram: ScoreBucket[];
  rules: RuleContribution[];
}

export interface SearchHit {
  kind: HitKind;
  label: string;
  sublabel: string;
  route: string;
  starred: boolean;
}

export interface SeriesPoint {
  bucket_start: string;
  count: number;
  sum: number;
}

export interface ServerConfig {
  version: string;
  oidc_enabled: boolean;
  oidc_required: boolean;
  registration_enabled: boolean;
  email_verification_enabled: boolean;
  smtp_enabled: boolean;
  quic: boolean;
  create_project: CreatePermission;
  create_team: CreatePermission;
  create_cache: CreatePermission;
  github_app_enabled: boolean;
  ssh_enabled: boolean;
  ssh_port?: number | null;
  gradient_ci_enabled: boolean;
  gradient_ci_url: string;
  logo_url?: string | null;
  exec_enabled: boolean;
}

export type SessionId = string;

export interface SessionInfo {
  id: string;
  user_agent?: string | null;
  ip?: string | null;
  created_at: string;
  last_used_at: string;
  expires_at: string;
  remember_me: boolean;
  current: boolean;
}

export interface SetDrainingRequest {
  enabled: boolean;
}

export interface SkippedTask {
  task_id: TaskId;
  task_name: string;
  project: string;
  reason: string;
}

export interface SourceFinalize {
  target?: string | null;
  system?: string | null;
  input_overrides?: InputOverrideBody[];
}

export interface SourceUploadForm {
  nar: string;
  target?: string | null;
}

export interface SshKeyInfo {
  id: UserSshKeyId;
  name: string;
  fingerprint: string;
  created_at: string;
  last_used_at?: string | null;
}

export interface StarredNames {
  projects: string[];
  tasks: StarredTask[];
  caches: string[];
}

export interface StarredTask {
  project: string;
  task: string;
}

export interface StartDeepGcResponse {
  task_id: AdminTaskId;
  status: string;
}

export interface StorageMetricPoint {
  time: string;
  packages: number;
  bytes: number;
}

export interface StoredNar {
  store_path: string;
  created: boolean;
}

export interface SubscribeCacheRequest {
  mode?: CacheSubscriptionMode | null;
}

export interface SubscriptionRequestItem {
  project: string;
  project_display_name: string;
  mode: CacheSubscriptionMode;
  requested_by: string;
  created_at: string;
}

export type SubscriptionStatus = "active" | "pending";

export interface SupervisedLoop {
  name: string;
  restarts: number;
  pass_errors: number;
  pass_timeouts: number;
  last_ok_seconds_ago?: number | null;
  last_error?: string | null;
}

export type TaskActionDeliveryId = string;

export type TaskActionId = string;

export interface TaskDetailsResponse {
  id: TaskId;
  name: string;
  display_name: string;
  description: string;
  repository: string;
  wildcard: string;
  active: boolean;
  created_at: string;
  keep_evaluations: number;
  last_check_at?: string | null;
  queue: QueueSummary;
  last_evaluations: EvaluationSummary[];
  can_edit: boolean;
  can_trigger: boolean;
  managed: boolean;
}

export type TaskId = string;

export interface TaskMetricPoint {
  evaluation_id: EvaluationId;
  created_at: string;
  build_time_total_ms: number;
  eval_time_ms: number;
  output_size_bytes?: number | null;
  closure_size_bytes?: number | null;
  runtime_closure_size_bytes?: number | null;
  dependencies_count: number;
}

export interface TaskMetricsResponse {
  keep_evaluations: number;
  points: TaskMetricPoint[];
}

export interface TaskResponse {
  id: TaskId;
  project: ProjectId;
  name: string;
  active: boolean;
  display_name: string;
  description: string;
  repository: string;
  wildcard: string;
  last_evaluation?: EvaluationId | null;
  last_evaluation_status?: EvaluationStatus | null;
  force_evaluation: boolean;
  created_by: UserId;
  created_at: string;
  managed: boolean;
  keep_evaluations: number;
  concurrency: ConcurrencyPolicy;
  sign_cache: boolean;
  wait_for_workers: boolean;
  retry_failed_builds: boolean;
  multi_node_evaluation: boolean;
  can_edit: boolean;
  can_trigger: boolean;
}

export interface TaskRow {
  project: string;
  task: string;
  starred: boolean;
  tier: Tier;
  latest?: Latest | null;
  entry_points?: Outcomes | null;
  delta?: number | null;
  speed_ms?: number | null;
  history: HistoryBar[];
}

export type TaskTriggerId = string;

export interface TasksPage {
  filter: Filter;
  counts: Counts;
  total: number;
  tasks: TaskRow[];
}

export interface TeamCacheGrant {
  cache: string;
  display_name: string;
  role?: string | null;
}

export interface TeamDemand {
  builds: UnmetRequirement[];
  network: number;
  secrets: number;
  eval: number;
}

export interface TeamEvaluation {
  id: EvaluationId;
  project: string;
  task: string;
  status: EvaluationStatus;
  created_at: string;
}

export interface TeamGrantEntry {
  team: string;
  display_name: string;
  role?: string | null;
  users: boolean;
  workers: boolean;
  pending: boolean;
}

export interface TeamGrants {
  projects: TeamProjectGrant[];
  caches: TeamCacheGrant[];
}

export type TeamId = string;

export interface TeamMemberItem {
  user: string;
  name: string;
  role: TeamRole;
  source: TeamMemberSource;
}

export interface TeamMemberRequest {
  user: string;
  role: TeamRole;
}

export type TeamMemberSource = "api" | "state" | "group";

export interface TeamProjectGrant {
  project: string;
  display_name: string;
  role?: string | null;
  users: boolean;
  workers: boolean;
}

export interface TeamRequestItem {
  id: string;
  kind: RequestKind;
  target: string;
  display_name: string;
  role?: string | null;
  users: boolean;
  workers: boolean;
  requested_by?: string | null;
  created_at: string;
}

export interface TeamResponse {
  id: TeamId;
  name: string;
  display_name: string;
  managed: boolean;
  role?: TeamRole | null;
  oidc_group?: string | null;
  scim_group?: string | null;
  new_project_users: boolean;
  new_project_workers: boolean;
  new_project_role?: string | null;
}

export type TeamRole = "admin" | "member";

export interface TeamSummary {
  name: string;
  display_name: string;
  role: TeamRole;
}

export type TeamWorkerEntry = WorkerConnection & {
  worker_id: string;
  display_name: string;
  registered_at: string;
  active: boolean;
  managed: boolean;
  url?: string | null;
  gradient_ci: boolean;
  enable_network: boolean;
  enable_secrets: boolean;
  enable_exec: boolean;
  enable_eval: boolean;
  enable_build: boolean;
  live?: WorkerLiveInfo | null;
};

export type Tier = "starred_active" | "active" | "starred" | "team" | "member";

export interface TopProjectBuildTime {
  project: string;
  project_name: string;
  project_display_name: string;
  total_build_ms: number;
  build_count: number;
}

export interface TransferOwnershipRequest {
  project: string;
}

export type TriggerConfig = {
  interval_secs: number;
  branch?: string | null;
  type: "polling";
} | {
  integration_id: IntegrationId;
  branches?: string[];
  tags?: string[];
  releases_only?: boolean;
  type: "reporter_push";
} | {
  integration_id: IntegrationId;
  branches?: string[];
  actions?: string[];
  require_approval?: boolean;
  type: "reporter_pull_request";
} | {
  cron: string;
  type: "time";
};

export interface TriggerIntegrationSummary {
  id: IntegrationId;
  name: string;
  display_name: string;
  git_host_type: string;
}

export interface TriggerOut {
  id: TaskTriggerId;
  task: TaskId;
  type: TriggerType;
  config: unknown;
  active: boolean;
  is_default: boolean;
  last_fired_at?: string | null;
  created_at: string;
  updated_at: string;
  integration?: TriggerIntegrationSummary | null;
}

export type TriggerType = "polling" | "reporter_push" | "reporter_pull_request" | "time";

export interface UnmetRequirement {
  architecture: string;
  required_features: string[];
  build_count: number;
}

export interface UpdateActionRequest {
  name?: string | null;
  config?: ActionConfig | null;
  events?: string[] | null;
  active?: boolean | null;
}

export interface UpdateFlakeInputOverrideRequest {
  input_name?: string | null;
  url?: string | null;
}

export interface UpdateTriggerRequest {
  config?: TriggerConfig | null;
  active?: boolean | null;
}

export interface UpdateWebhookRequest {
  name?: string | null;
  url?: string | null;
  events?: string[] | null;
  active?: boolean | null;
}

export type UploadSessionId = string;

export interface UpstreamCacheItem {
  id: CacheUpstreamId;
  display_name: string;
  mode: CacheSubscriptionMode;
  upstream_cache_id?: CacheId | null;
  url?: string | null;
  public_key?: string | null;
  kind: string;
  remote_cache?: string | null;
  http1_only: boolean;
  active: boolean;
}

export interface UpstreamTestResponse {
  ok: boolean;
  http1: ProtocolProbe;
  http2: ProtocolProbe;
  message: string;
}

export interface UrlRequest {
  project: string;
  url: string;
  ref?: string | null;
  rev?: string | null;
  target?: string | null;
  input_overrides?: InputOverrideBody[];
}

export type UserId = string;

export interface UserInfoResponse {
  id: string;
  username: string;
  name: string;
  email: string;
  superuser: boolean;
}

export interface UserRequest {
  userName: string;
  externalId?: string | null;
  name?: NameRequest | null;
  displayName?: string | null;
  emails?: EmailResource[];
  active?: boolean;
}

export interface UserResource {
  schemas: string[];
  id: string;
  userName: string;
  externalId?: string | null;
  name: NameResource;
  displayName: string;
  emails: EmailResource[];
  active: boolean;
  meta: Meta;
}

export interface UserSearchResult {
  id: string;
  username: string;
  name: string;
}

export type UserSshKeyId = string;

export type Value = unknown;

export type VerifyGate = "none" | "eval" | "build";

export type WaitingReason = {
  unmet: UnmetRequirement[];
  connected_workers: number;
  available_architectures: string[];
  kind: "workers";
} | {
  capability: EvalCapability;
  connected_workers: number;
  kind: "eval_workers";
} | {
  pr_number: number;
  pr_author: string;
  kind: "approval";
} | {
  kind: "no_cache";
} | {
  kind: "cache_storage_full";
} | {
  kind: "draining";
} | {
  pending_shared_builds: number;
  kind: "graph_stuck";
} | {
  pending_shared_builds: number;
  failing_upstream_caches: string[];
  kind: "upstream_caches";
} | {
  unmet: ExecUnmet;
  connected_workers: number;
  matching_workers: number;
  kind: "exec_workers";
};

export type WalkMode = "pruned" | "full";

export type WebhookDeliveryId = string;

export type WebhookId = string;

export interface WebhookResponse {
  id: WebhookId;
  scope: WebhookScope;
  name: string;
  url: string;
  events: string[];
  active: boolean;
  last_fired_at?: string | null;
  created_by: UserId;
  created_at: string;
  updated_at: string;
}

export type WebhookScope = "project" | "cache" | "instance" | "team";

export interface WorkerConnection {
  connected: boolean;
  last_error?: ConnectionFailure | null;
}

export interface WorkerConnectionEntry {
  connected_at: string;
  disconnected_at?: string | null;
}

export interface WorkerInfo {
  id: string;
  capabilities: GradientCapabilities;
  architectures: string[];
  system_features: string[];
  max_concurrent_builds: number;
  assigned_job_count: number;
  draining: boolean;
  authorized_peers?: ProjectId[] | null;
  exec?: ExecInventory | null;
}

export interface WorkerLiveInfo {
  capabilities: GradientCapabilities;
  architectures: string[];
  system_features: string[];
  max_concurrent_builds: number;
  assigned_job_count: number;
  draining: boolean;
  ram_total_mb?: number | null;
}

export interface WorkerLoad {
  by_capability: LoadBucket[];
  by_architecture: LoadBucket[];
  by_feature: LoadBucket[];
}

export interface WorkerMetricsResponse {
  worker_id: string;
  display_name: string;
  samples: WorkerSamplePoint[];
  connections: WorkerConnectionEntry[];
  jobs_dispatched: number;
}

export interface WorkerNet {
  worker_id?: string | null;
  worker_name?: string | null;
  upload_speed_mbps?: number | null;
  download_speed_mbps?: number | null;
  disk_speed_mbps?: number | null;
}

export interface WorkerSamplePoint {
  at: string;
  cpu_usage_pct?: number | null;
  ram_free_mb?: number | null;
  ram_total_mb?: number | null;
  disk_speed_mbps?: number | null;
  upload_speed_mbps?: number | null;
  download_speed_mbps?: number | null;
  assigned_jobs: number;
  max_concurrent_builds: number;
  state: number;
}

export interface WorkerTestResponse {
  ok: boolean;
  connected: boolean;
  authorized_for_project: boolean;
  message: string;
}
