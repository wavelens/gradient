--
-- PostgreSQL database dump
--

-- Dumped from database version 18.4
-- Dumped by pg_dump version 18.4

--
-- Name: acknowledged_derivation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.acknowledged_derivation (
    id uuid NOT NULL,
    derivation uuid,
    pname character varying,
    note text NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: admin_task; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.admin_task (
    id uuid NOT NULL,
    kind integer NOT NULL,
    status integer NOT NULL,
    created_at timestamp without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    started_at timestamp without time zone,
    finished_at timestamp without time zone,
    progress jsonb,
    error text,
    created_by uuid
);

--
-- Name: api; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.api (
    id uuid NOT NULL,
    owned_by uuid NOT NULL,
    name character varying NOT NULL,
    key character varying NOT NULL,
    last_used_at timestamp without time zone NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    expires_at timestamp without time zone,
    revoked_at timestamp without time zone,
    permission bigint DEFAULT 8191 NOT NULL,
    organization uuid,
    cache uuid,
    allowed_ips text[]
);

--
-- Name: audit_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_log (
    id uuid NOT NULL,
    user_id uuid,
    event character varying NOT NULL,
    ip character varying,
    user_agent text,
    metadata json,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: base_worker; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.base_worker (
    id uuid NOT NULL,
    worker_id character varying NOT NULL,
    token_hash character varying NOT NULL,
    url character varying,
    display_name text NOT NULL,
    enable_fetch boolean DEFAULT true NOT NULL,
    enable_eval boolean DEFAULT true NOT NULL,
    enable_build boolean DEFAULT true NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    authorize_against uuid,
    created_by uuid,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: build; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    status integer NOT NULL,
    log text,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    derivation uuid NOT NULL,
    via uuid,
    attempt integer DEFAULT 0 NOT NULL,
    timeout_secs bigint,
    max_silent_secs bigint,
    prefer_local_build boolean DEFAULT false NOT NULL,
    ready_at timestamp without time zone,
    dispatched_at timestamp without time zone,
    queued_at timestamp without time zone,
    substitutable boolean DEFAULT false NOT NULL,
    substituted boolean DEFAULT false NOT NULL
);

--
-- Name: build_attempt; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_attempt (
    id uuid NOT NULL,
    build uuid NOT NULL,
    dispatched_job uuid NOT NULL,
    substitute boolean DEFAULT false NOT NULL,
    outcome integer DEFAULT 0 NOT NULL,
    reason integer,
    failure_message text,
    log_id uuid,
    build_context jsonb NOT NULL,
    build_started_at timestamp without time zone,
    build_finished_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: build_log_chunk; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_log_chunk (
    id uuid NOT NULL,
    build uuid NOT NULL,
    chunk_index integer NOT NULL,
    byte_start bigint NOT NULL,
    byte_len integer NOT NULL,
    line_start bigint NOT NULL,
    line_count integer NOT NULL,
    compressed_size integer NOT NULL,
    color_prefix text NOT NULL
);

--
-- Name: build_product; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_product (
    id uuid NOT NULL,
    derivation_output uuid NOT NULL,
    file_type character varying NOT NULL,
    name character varying NOT NULL,
    path character varying NOT NULL,
    size bigint,
    created_at timestamp without time zone NOT NULL,
    subtype text DEFAULT ''::text NOT NULL
);

--
-- Name: build_request_blob; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_request_blob (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    hash bytea NOT NULL,
    size bigint NOT NULL,
    created_at timestamp without time zone NOT NULL,
    last_used_at timestamp without time zone NOT NULL
);

--
-- Name: cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache (
    id uuid NOT NULL,
    name character varying NOT NULL,
    display_name character varying NOT NULL,
    description text NOT NULL,
    active boolean NOT NULL,
    priority integer NOT NULL,
    private_key character varying CONSTRAINT cache_signing_key_not_null NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    public boolean DEFAULT false NOT NULL,
    public_key character varying DEFAULT ''::character varying NOT NULL,
    local_priority integer,
    max_storage_gb integer DEFAULT 0 NOT NULL
);

--
-- Name: cache_derivation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_derivation (
    id uuid NOT NULL,
    cache uuid NOT NULL,
    derivation uuid NOT NULL,
    cached_at timestamp without time zone NOT NULL,
    last_fetched_at timestamp without time zone
);

--
-- Name: cache_metric; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_metric (
    id uuid NOT NULL,
    cache uuid NOT NULL,
    bucket_time timestamp without time zone NOT NULL,
    bytes_sent bigint DEFAULT 0 NOT NULL,
    nar_count integer DEFAULT 0 NOT NULL
);

--
-- Name: cache_role; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_role (
    id uuid NOT NULL,
    name character varying NOT NULL,
    cache uuid,
    permission bigint NOT NULL,
    managed boolean DEFAULT false NOT NULL
);

--
-- Name: cache_upstream; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_upstream (
    id uuid NOT NULL,
    cache uuid NOT NULL,
    display_name character varying NOT NULL,
    mode integer DEFAULT 0 NOT NULL,
    upstream_cache uuid,
    url character varying,
    public_key character varying,
    kind integer DEFAULT 2 NOT NULL,
    remote_cache_name text,
    api_key text
);

--
-- Name: cache_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_user (
    id uuid NOT NULL,
    cache uuid NOT NULL,
    "user" uuid NOT NULL,
    role uuid NOT NULL
);

--
-- Name: cached_path; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cached_path (
    id uuid NOT NULL,
    hash character varying NOT NULL,
    package text NOT NULL,
    file_hash text,
    file_size bigint,
    nar_size bigint,
    nar_hash text,
    "references" text,
    ca text,
    created_at timestamp without time zone NOT NULL,
    deriver text
);

--
-- Name: cached_path_signature; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cached_path_signature (
    id uuid NOT NULL,
    cached_path uuid NOT NULL,
    cache uuid NOT NULL,
    signature bytea,
    created_at timestamp without time zone NOT NULL,
    last_fetched_at timestamp without time zone,
    fetch_count bigint DEFAULT 0 NOT NULL
);

--
-- Name: cli_device_authorization; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cli_device_authorization (
    id uuid NOT NULL,
    device_code_hash character varying NOT NULL,
    user_code character varying NOT NULL,
    user_id uuid,
    token text,
    denied_at timestamp without time zone,
    authorized_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL,
    user_agent text,
    ip character varying
);

--
-- Name: commit; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.commit (
    id uuid NOT NULL,
    message character varying NOT NULL,
    hash bytea NOT NULL,
    author uuid,
    author_name character varying NOT NULL
);

--
-- Name: derivation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    architecture text DEFAULT ''::text CONSTRAINT derivation_architecture_text_not_null NOT NULL,
    hash text NOT NULL,
    name text NOT NULL,
    pname character varying,
    prefer_local_build boolean DEFAULT false NOT NULL,
    allow_substitutes boolean DEFAULT true NOT NULL,
    closure_size bigint,
    is_fixed_output boolean DEFAULT false NOT NULL,
    dep_closure_count bigint
);

--
-- Name: derivation_closure; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_closure (
    id uuid NOT NULL,
    root_derivation uuid NOT NULL,
    dep_derivation uuid NOT NULL
);

--
-- Name: derivation_dependency; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_dependency (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    dependency uuid NOT NULL
);

--
-- Name: derivation_feature; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_feature (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    feature uuid NOT NULL
);

--
-- Name: derivation_metric; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_metric (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    pname character varying,
    closure_size bigint,
    peak_ram_mb bigint,
    cpu_time_ms bigint,
    avg_cpu_pct double precision,
    disk_read_bytes bigint,
    disk_write_bytes bigint,
    oom_killed boolean DEFAULT false NOT NULL,
    build_time_ms bigint,
    worker_id character varying NOT NULL,
    created_at timestamp without time zone NOT NULL,
    peak_network_mbps double precision
);

--
-- Name: derivation_output; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_output (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    name character varying NOT NULL,
    hash character varying NOT NULL,
    package character varying NOT NULL,
    ca character varying,
    nar_size bigint,
    is_cached boolean DEFAULT false NOT NULL,
    created_at timestamp without time zone NOT NULL,
    cached_path uuid
);

--
-- Name: dispatched_job; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.dispatched_job (
    id uuid NOT NULL,
    kind smallint NOT NULL,
    evaluation_id uuid NOT NULL,
    organization uuid NOT NULL,
    project uuid,
    worker_id character varying NOT NULL,
    score double precision DEFAULT 0 NOT NULL,
    queued_at timestamp without time zone NOT NULL,
    ready_at timestamp without time zone,
    dispatched_at timestamp without time zone NOT NULL,
    finished_at timestamp without time zone,
    score_breakdown jsonb NOT NULL,
    worker_context jsonb NOT NULL,
    job_context jsonb NOT NULL,
    candidates jsonb,
    created_at timestamp without time zone NOT NULL,
    instance_context jsonb
);

--
-- Name: entry_point; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.entry_point (
    id uuid NOT NULL,
    project uuid NOT NULL,
    evaluation uuid NOT NULL,
    build uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    eval character varying DEFAULT ''::character varying NOT NULL,
    repo_check_id bigint
);

--
-- Name: entry_point_dep_count; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.entry_point_dep_count (
    id uuid NOT NULL,
    entry_point uuid NOT NULL,
    status integer NOT NULL,
    count bigint DEFAULT 0 NOT NULL
);

--
-- Name: entry_point_message; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.entry_point_message (
    id uuid NOT NULL,
    entry_point uuid NOT NULL,
    message uuid NOT NULL
);

--
-- Name: eval_cache_store; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.eval_cache_store (
    id uuid NOT NULL,
    fingerprint character varying NOT NULL,
    storage_path text NOT NULL,
    size_bytes bigint NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);

--
-- Name: evaluation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation (
    id uuid NOT NULL,
    project uuid,
    repository character varying NOT NULL,
    commit uuid NOT NULL,
    wildcard character varying NOT NULL,
    status integer NOT NULL,
    previous uuid,
    next uuid,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone DEFAULT now() NOT NULL,
    flake_source text,
    waiting_reason jsonb,
    trigger uuid,
    concurrent boolean DEFAULT false NOT NULL,
    check_run_ids jsonb,
    source_comment jsonb,
    fetch_started_at timestamp without time zone,
    eval_flake_started_at timestamp without time zone,
    eval_drv_started_at timestamp without time zone,
    building_started_at timestamp without time zone,
    finished_at timestamp without time zone,
    started_by uuid,
    cache_status integer DEFAULT 0 NOT NULL,
    kind integer DEFAULT 0 NOT NULL
);

--
-- Name: evaluation_attr_cost; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_attr_cost (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    attr character varying NOT NULL,
    thunks bigint NOT NULL,
    fn_calls bigint NOT NULL,
    eval_ms bigint NOT NULL,
    alloc_bytes bigint NOT NULL
);

--
-- Name: evaluation_flake_input_override; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_flake_input_override (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    input_name text NOT NULL,
    url text
);

--
-- Name: evaluation_input_update; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_input_update (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    base_commit text NOT NULL,
    generator text NOT NULL,
    target_inputs jsonb NOT NULL,
    candidate_lock text,
    bumped_inputs jsonb,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);

--
-- Name: evaluation_message; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_message (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    level integer NOT NULL,
    message text NOT NULL,
    source character varying,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: evaluation_metric; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_metric (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    total_thunks bigint NOT NULL,
    fn_calls bigint NOT NULL,
    primop_calls bigint NOT NULL,
    lookups bigint NOT NULL,
    alloc_bytes bigint NOT NULL,
    peak_heap_mb bigint NOT NULL,
    peak_rss_mb bigint NOT NULL,
    fetch_ms bigint NOT NULL,
    eval_flake_ms bigint NOT NULL,
    eval_drv_ms bigint NOT NULL,
    total_eval_ms bigint NOT NULL,
    worker_id character varying NOT NULL,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: flake_output_node; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.flake_output_node (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    path character varying NOT NULL,
    parent character varying,
    name character varying NOT NULL,
    kind character varying NOT NULL,
    is_derivation boolean NOT NULL,
    drv_path character varying
);

--
-- Name: integration; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.integration (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    name character varying NOT NULL,
    kind smallint NOT NULL,
    forge_type smallint NOT NULL,
    secret text,
    endpoint_url text,
    access_token text,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    display_name character varying DEFAULT ''::character varying NOT NULL,
    allowed_ips text[]
);

--
-- Name: metric_rollup; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.metric_rollup (
    id uuid NOT NULL,
    metric character varying NOT NULL,
    granularity smallint NOT NULL,
    bucket_start timestamp without time zone NOT NULL,
    scope jsonb NOT NULL,
    scope_hash bigint NOT NULL,
    count bigint DEFAULT 0 NOT NULL,
    sum double precision DEFAULT 0 NOT NULL,
    min double precision DEFAULT 0 NOT NULL,
    max double precision DEFAULT 0 NOT NULL,
    sum_sq double precision DEFAULT 0 NOT NULL,
    histogram jsonb
);

--
-- Name: open_pr_state; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.open_pr_state (
    id uuid NOT NULL,
    project uuid NOT NULL,
    action uuid NOT NULL,
    branch text NOT NULL,
    forge_pr_number bigint,
    head_commit text,
    status text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);

--
-- Name: organization; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.organization (
    id uuid NOT NULL,
    name character varying NOT NULL,
    display_name character varying NOT NULL,
    description text NOT NULL,
    public_key character varying NOT NULL,
    private_key character varying NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    public boolean DEFAULT false NOT NULL,
    github_installation_id bigint,
    hide_build_requests boolean DEFAULT false NOT NULL
);

--
-- Name: organization_base_worker; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.organization_base_worker (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    base_worker uuid NOT NULL,
    created_by uuid,
    created_at timestamp without time zone NOT NULL
);

--
-- Name: organization_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.organization_cache (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    cache uuid NOT NULL,
    mode integer DEFAULT 0 NOT NULL
);

--
-- Name: organization_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.organization_user (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    "user" uuid NOT NULL,
    role uuid NOT NULL
);

--
-- Name: phase_event; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.phase_event (
    id uuid NOT NULL,
    subject_kind smallint NOT NULL,
    subject_id uuid NOT NULL,
    phase smallint NOT NULL,
    event smallint NOT NULL,
    at timestamp without time zone NOT NULL,
    worker_id character varying,
    detail jsonb
);

--
-- Name: project; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    name character varying NOT NULL,
    active boolean NOT NULL,
    display_name character varying NOT NULL,
    description text NOT NULL,
    repository character varying NOT NULL,
    wildcard character varying CONSTRAINT project_evaluation_wildcard_not_null NOT NULL,
    last_evaluation uuid,
    last_check_at timestamp without time zone NOT NULL,
    force_evaluation boolean NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    keep_evaluations integer DEFAULT 30 NOT NULL,
    concurrency smallint DEFAULT 1 NOT NULL,
    sign_cache boolean DEFAULT true NOT NULL
);

--
-- Name: project_action; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_action (
    id uuid NOT NULL,
    project uuid NOT NULL,
    name character varying NOT NULL,
    action_type smallint NOT NULL,
    config jsonb NOT NULL,
    events jsonb NOT NULL,
    active boolean DEFAULT true NOT NULL,
    last_fired_at timestamp without time zone,
    created_by uuid NOT NULL,
    created_at timestamp without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    updated_at timestamp without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

--
-- Name: project_action_delivery; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_action_delivery (
    id uuid NOT NULL,
    action_id uuid NOT NULL,
    event character varying NOT NULL,
    request_body text NOT NULL,
    response_status integer,
    response_body text,
    error_message text,
    success boolean NOT NULL,
    duration_ms integer NOT NULL,
    delivered_at timestamp without time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

--
-- Name: project_flake_input_override; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_flake_input_override (
    id uuid NOT NULL,
    project uuid NOT NULL,
    input_name text NOT NULL,
    url text,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);

--
-- Name: project_trigger; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_trigger (
    id uuid NOT NULL,
    project uuid NOT NULL,
    trigger_type smallint NOT NULL,
    config jsonb NOT NULL,
    active boolean DEFAULT true NOT NULL,
    last_fired_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);

--
-- Name: role; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.role (
    id uuid NOT NULL,
    name character varying NOT NULL,
    organization uuid,
    permission bigint NOT NULL,
    managed boolean DEFAULT false NOT NULL
);

--
-- Name: session; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.session (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL,
    last_used_at timestamp without time zone NOT NULL,
    revoked_at timestamp without time zone,
    user_agent text,
    ip character varying,
    remember_me boolean DEFAULT false NOT NULL
);

--
-- Name: system_requirement; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.system_requirement (
    id uuid CONSTRAINT feature_id_not_null NOT NULL,
    name character varying CONSTRAINT feature_name_not_null NOT NULL,
    kind character varying DEFAULT 'feature'::character varying NOT NULL
);

--
-- Name: upload_session; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.upload_session (
    id uuid NOT NULL,
    organization uuid NOT NULL,
    manifest jsonb NOT NULL,
    missing jsonb NOT NULL,
    total_size bigint NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL,
    dispatched_at timestamp without time zone
);

--
-- Name: user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public."user" (
    id uuid NOT NULL,
    username character varying NOT NULL,
    name character varying NOT NULL,
    email character varying NOT NULL,
    password character varying,
    last_login_at timestamp without time zone NOT NULL,
    created_at timestamp without time zone NOT NULL,
    email_verified boolean DEFAULT false NOT NULL,
    email_verification_token character varying,
    email_verification_token_expires timestamp without time zone,
    managed boolean DEFAULT false NOT NULL,
    superuser boolean DEFAULT false NOT NULL,
    oidc_issuer character varying,
    oidc_subject character varying,
    active boolean DEFAULT true NOT NULL,
    scim_external_id character varying
);

--
-- Name: worker_connection; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.worker_connection (
    id uuid NOT NULL,
    worker_id character varying NOT NULL,
    organization uuid NOT NULL,
    display_name character varying NOT NULL,
    connected_at timestamp without time zone NOT NULL,
    disconnected_at timestamp without time zone,
    capabilities jsonb NOT NULL,
    reason smallint
);

--
-- Name: worker_registration; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.worker_registration (
    id uuid NOT NULL,
    peer_id uuid NOT NULL,
    worker_id character varying NOT NULL,
    token_hash character varying NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    url character varying,
    active boolean DEFAULT true NOT NULL,
    display_name text DEFAULT 'worker'::text CONSTRAINT worker_registration_name_not_null NOT NULL,
    created_by uuid,
    enable_fetch boolean DEFAULT true NOT NULL,
    enable_eval boolean DEFAULT true NOT NULL,
    enable_build boolean DEFAULT true NOT NULL
);

--
-- Name: worker_sample; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.worker_sample (
    id uuid NOT NULL,
    worker_id character varying NOT NULL,
    organization uuid NOT NULL,
    at timestamp without time zone NOT NULL,
    cpu_usage_pct real,
    ram_free_mb bigint,
    ram_total_mb bigint,
    disk_speed_mbps real,
    network_speed_mbps real,
    assigned_jobs integer DEFAULT 0 NOT NULL,
    max_concurrent_builds integer DEFAULT 0 NOT NULL,
    state smallint DEFAULT 0 NOT NULL,
    capabilities jsonb NOT NULL
);

--
-- Name: acknowledged_derivation acknowledged_derivation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.acknowledged_derivation
    ADD CONSTRAINT acknowledged_derivation_pkey PRIMARY KEY (id);

--
-- Name: admin_task admin_task_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.admin_task
    ADD CONSTRAINT admin_task_pkey PRIMARY KEY (id);

--
-- Name: api api_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.api
    ADD CONSTRAINT api_pkey PRIMARY KEY (id);

--
-- Name: audit_log audit_log_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_log
    ADD CONSTRAINT audit_log_pkey PRIMARY KEY (id);

--
-- Name: base_worker base_worker_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.base_worker
    ADD CONSTRAINT base_worker_pkey PRIMARY KEY (id);

--
-- Name: build_attempt build_attempt_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_attempt
    ADD CONSTRAINT build_attempt_pkey PRIMARY KEY (id);

--
-- Name: build_log_chunk build_log_chunk_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_log_chunk
    ADD CONSTRAINT build_log_chunk_pkey PRIMARY KEY (id);

--
-- Name: build build_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build
    ADD CONSTRAINT build_pkey PRIMARY KEY (id);

--
-- Name: build_product build_product_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_product
    ADD CONSTRAINT build_product_pkey PRIMARY KEY (id);

--
-- Name: build_request_blob build_request_blob_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_request_blob
    ADD CONSTRAINT build_request_blob_pkey PRIMARY KEY (id);

--
-- Name: cache_derivation cache_derivation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_derivation
    ADD CONSTRAINT cache_derivation_pkey PRIMARY KEY (id);

--
-- Name: cache_metric cache_metric_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_metric
    ADD CONSTRAINT cache_metric_pkey PRIMARY KEY (id);

--
-- Name: cache cache_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache
    ADD CONSTRAINT cache_name_key UNIQUE (name);

--
-- Name: cache cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache
    ADD CONSTRAINT cache_pkey PRIMARY KEY (id);

--
-- Name: cache_role cache_role_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_role
    ADD CONSTRAINT cache_role_pkey PRIMARY KEY (id);

--
-- Name: cache_upstream cache_upstream_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_upstream
    ADD CONSTRAINT cache_upstream_pkey PRIMARY KEY (id);

--
-- Name: cache_user cache_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT cache_user_pkey PRIMARY KEY (id);

--
-- Name: cached_path cached_path_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cached_path
    ADD CONSTRAINT cached_path_hash_key UNIQUE (hash);

--
-- Name: cached_path cached_path_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cached_path
    ADD CONSTRAINT cached_path_pkey PRIMARY KEY (id);

--
-- Name: cached_path_signature cached_path_signature_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cached_path_signature
    ADD CONSTRAINT cached_path_signature_pkey PRIMARY KEY (id);

--
-- Name: cli_device_authorization cli_device_authorization_device_code_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cli_device_authorization
    ADD CONSTRAINT cli_device_authorization_device_code_hash_key UNIQUE (device_code_hash);

--
-- Name: cli_device_authorization cli_device_authorization_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cli_device_authorization
    ADD CONSTRAINT cli_device_authorization_pkey PRIMARY KEY (id);

--
-- Name: cli_device_authorization cli_device_authorization_user_code_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cli_device_authorization
    ADD CONSTRAINT cli_device_authorization_user_code_key UNIQUE (user_code);

--
-- Name: commit commit_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.commit
    ADD CONSTRAINT commit_pkey PRIMARY KEY (id);

--
-- Name: derivation_closure derivation_closure_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_closure
    ADD CONSTRAINT derivation_closure_pkey PRIMARY KEY (id);

--
-- Name: derivation_dependency derivation_dependency_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_dependency
    ADD CONSTRAINT derivation_dependency_pkey PRIMARY KEY (id);

--
-- Name: derivation_feature derivation_feature_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_feature
    ADD CONSTRAINT derivation_feature_pkey PRIMARY KEY (id);

--
-- Name: derivation_metric derivation_metric_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_metric
    ADD CONSTRAINT derivation_metric_pkey PRIMARY KEY (id);

--
-- Name: derivation_output derivation_output_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_output
    ADD CONSTRAINT derivation_output_pkey PRIMARY KEY (id);

--
-- Name: derivation derivation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation
    ADD CONSTRAINT derivation_pkey PRIMARY KEY (id);

--
-- Name: dispatched_job dispatched_job_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dispatched_job
    ADD CONSTRAINT dispatched_job_pkey PRIMARY KEY (id);

--
-- Name: entry_point_dep_count entry_point_dep_count_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point_dep_count
    ADD CONSTRAINT entry_point_dep_count_pkey PRIMARY KEY (id);

--
-- Name: entry_point_message entry_point_message_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point_message
    ADD CONSTRAINT entry_point_message_pkey PRIMARY KEY (id);

--
-- Name: entry_point entry_point_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT entry_point_pkey PRIMARY KEY (id);

--
-- Name: eval_cache_store eval_cache_store_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.eval_cache_store
    ADD CONSTRAINT eval_cache_store_pkey PRIMARY KEY (id);

--
-- Name: evaluation_attr_cost evaluation_attr_cost_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_attr_cost
    ADD CONSTRAINT evaluation_attr_cost_pkey PRIMARY KEY (id);

--
-- Name: evaluation_flake_input_override evaluation_flake_input_override_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_flake_input_override
    ADD CONSTRAINT evaluation_flake_input_override_pkey PRIMARY KEY (id);

--
-- Name: evaluation_input_update evaluation_input_update_evaluation_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_input_update
    ADD CONSTRAINT evaluation_input_update_evaluation_key UNIQUE (evaluation);

--
-- Name: evaluation_input_update evaluation_input_update_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_input_update
    ADD CONSTRAINT evaluation_input_update_pkey PRIMARY KEY (id);

--
-- Name: evaluation_message evaluation_message_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_message
    ADD CONSTRAINT evaluation_message_pkey PRIMARY KEY (id);

--
-- Name: evaluation_metric evaluation_metric_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_metric
    ADD CONSTRAINT evaluation_metric_pkey PRIMARY KEY (id);

--
-- Name: evaluation evaluation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT evaluation_pkey PRIMARY KEY (id);

--
-- Name: system_requirement feature_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_requirement
    ADD CONSTRAINT feature_pkey PRIMARY KEY (id);

--
-- Name: flake_output_node flake_output_node_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flake_output_node
    ADD CONSTRAINT flake_output_node_pkey PRIMARY KEY (id);

--
-- Name: integration integration_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT integration_pkey PRIMARY KEY (id);

--
-- Name: metric_rollup metric_rollup_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.metric_rollup
    ADD CONSTRAINT metric_rollup_pkey PRIMARY KEY (id);

--
-- Name: open_pr_state open_pr_state_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.open_pr_state
    ADD CONSTRAINT open_pr_state_pkey PRIMARY KEY (id);

--
-- Name: organization_base_worker organization_base_worker_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_base_worker
    ADD CONSTRAINT organization_base_worker_pkey PRIMARY KEY (id);

--
-- Name: organization_cache organization_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_cache
    ADD CONSTRAINT organization_cache_pkey PRIMARY KEY (id);

--
-- Name: organization organization_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization
    ADD CONSTRAINT organization_name_key UNIQUE (name);

--
-- Name: organization organization_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization
    ADD CONSTRAINT organization_pkey PRIMARY KEY (id);

--
-- Name: organization_user organization_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_user
    ADD CONSTRAINT organization_user_pkey PRIMARY KEY (id);

--
-- Name: phase_event phase_event_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.phase_event
    ADD CONSTRAINT phase_event_pkey PRIMARY KEY (id);

--
-- Name: project_action_delivery project_action_delivery_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_action_delivery
    ADD CONSTRAINT project_action_delivery_pkey PRIMARY KEY (id);

--
-- Name: project_action project_action_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_action
    ADD CONSTRAINT project_action_pkey PRIMARY KEY (id);

--
-- Name: project_flake_input_override project_flake_input_override_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_flake_input_override
    ADD CONSTRAINT project_flake_input_override_pkey PRIMARY KEY (id);

--
-- Name: project project_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT project_pkey PRIMARY KEY (id);

--
-- Name: project_trigger project_trigger_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_trigger
    ADD CONSTRAINT project_trigger_pkey PRIMARY KEY (id);

--
-- Name: role role_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role
    ADD CONSTRAINT role_pkey PRIMARY KEY (id);

--
-- Name: session session_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session
    ADD CONSTRAINT session_pkey PRIMARY KEY (id);

--
-- Name: system_requirement system_requirement_name_kind_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_requirement
    ADD CONSTRAINT system_requirement_name_kind_unique UNIQUE (name, kind);

--
-- Name: upload_session upload_session_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upload_session
    ADD CONSTRAINT upload_session_pkey PRIMARY KEY (id);

--
-- Name: cache_user uq-cache_user-cache-user; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT "uq-cache_user-cache-user" UNIQUE (cache, "user");

--
-- Name: user user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT user_pkey PRIMARY KEY (id);

--
-- Name: user user_username_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT user_username_key UNIQUE (username);

--
-- Name: worker_connection worker_connection_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.worker_connection
    ADD CONSTRAINT worker_connection_pkey PRIMARY KEY (id);

--
-- Name: worker_registration worker_registration_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.worker_registration
    ADD CONSTRAINT worker_registration_pkey PRIMARY KEY (id);

--
-- Name: worker_sample worker_sample_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.worker_sample
    ADD CONSTRAINT worker_sample_pkey PRIMARY KEY (id);

--
-- Name: admin_task_one_active_per_kind; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX admin_task_one_active_per_kind ON public.admin_task USING btree (kind) WHERE (status = ANY (ARRAY[0, 1]));

--
-- Name: idx-acknowledged_derivation-pname; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-acknowledged_derivation-pname" ON public.acknowledged_derivation USING btree (pname);

--
-- Name: idx-build-derivation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build-derivation" ON public.build USING btree (derivation);

--
-- Name: idx-build-evaluation-derivation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build-evaluation-derivation" ON public.build USING btree (evaluation, derivation);

--
-- Name: idx-build-ready-queue; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build-ready-queue" ON public.build USING btree (updated_at) WHERE ((status = 1) AND (via IS NULL));

--
-- Name: idx-build-via; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build-via" ON public.build USING btree (via);

--
-- Name: idx-build_product-derivation_output; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_product-derivation_output" ON public.build_product USING btree (derivation_output);

--
-- Name: idx-cache_derivation-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cache_derivation-pair" ON public.cache_derivation USING btree (cache, derivation);

--
-- Name: idx-cache_metric-cache-bucket_time; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cache_metric-cache-bucket_time" ON public.cache_metric USING btree (cache, bucket_time);

--
-- Name: idx-cached_path-hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path-hash" ON public.cached_path USING btree (hash);

--
-- Name: idx-cached_path_signature-cache; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path_signature-cache" ON public.cached_path_signature USING btree (cache);

--
-- Name: idx-cached_path_signature-cache-last_fetched_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path_signature-cache-last_fetched_at" ON public.cached_path_signature USING btree (cache, last_fetched_at DESC NULLS LAST);

--
-- Name: idx-cached_path_signature-cached_path-cache; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cached_path_signature-cached_path-cache" ON public.cached_path_signature USING btree (cached_path, cache);

--
-- Name: idx-derivation-org-hash-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation-org-hash-name" ON public.derivation USING btree (organization, hash, name);

--
-- Name: idx-derivation_closure-dep; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_closure-dep" ON public.derivation_closure USING btree (dep_derivation);

--
-- Name: idx-derivation_closure-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation_closure-pair" ON public.derivation_closure USING btree (root_derivation, dep_derivation);

--
-- Name: idx-derivation_dependency-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation_dependency-pair" ON public.derivation_dependency USING btree (derivation, dependency);

--
-- Name: idx-derivation_feature-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation_feature-pair" ON public.derivation_feature USING btree (derivation, feature);

--
-- Name: idx-derivation_metric-pname-closure_size; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_metric-pname-closure_size" ON public.derivation_metric USING btree (pname, closure_size);

--
-- Name: idx-derivation_output-derivation-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation_output-derivation-name" ON public.derivation_output USING btree (derivation, name);

--
-- Name: idx-derivation_output-hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_output-hash" ON public.derivation_output USING btree (hash);

--
-- Name: idx-derivation_output-hash-cached; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_output-hash-cached" ON public.derivation_output USING btree (hash) WHERE (is_cached = true);

--
-- Name: idx-dispatched_job-open; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-open" ON public.dispatched_job USING btree (dispatched_at DESC) WHERE (finished_at IS NULL);

--
-- Name: idx-dispatched_job-org-dispatched_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-org-dispatched_at" ON public.dispatched_job USING btree (organization, dispatched_at DESC);

--
-- Name: idx-dispatched_job-worker-dispatched_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-worker-dispatched_at" ON public.dispatched_job USING btree (worker_id, dispatched_at DESC);

--
-- Name: idx-entry_point_dep_count-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-entry_point_dep_count-pair" ON public.entry_point_dep_count USING btree (entry_point, status);

--
-- Name: idx-entry_point_message-unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-entry_point_message-unique" ON public.entry_point_message USING btree (entry_point, message);

--
-- Name: idx-eval_cache_store-fingerprint; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-eval_cache_store-fingerprint" ON public.eval_cache_store USING btree (fingerprint);

--
-- Name: idx-evaluation_attr_cost-attr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_attr_cost-attr" ON public.evaluation_attr_cost USING btree (attr);

--
-- Name: idx-evaluation_attr_cost-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_attr_cost-evaluation" ON public.evaluation_attr_cost USING btree (evaluation);

--
-- Name: idx-evaluation_flake_input_override-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_flake_input_override-evaluation" ON public.evaluation_flake_input_override USING btree (evaluation);

--
-- Name: idx-evaluation_message-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_message-evaluation" ON public.evaluation_message USING btree (evaluation);

--
-- Name: idx-evaluation_metric-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_metric-evaluation" ON public.evaluation_metric USING btree (evaluation);

--
-- Name: idx-flake_output_node-evaluation-parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-flake_output_node-evaluation-parent" ON public.flake_output_node USING btree (evaluation, parent);

--
-- Name: idx-integration-org-kind-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-integration-org-kind-name" ON public.integration USING btree (organization, kind, name);

--
-- Name: idx-metric_rollup-query; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-metric_rollup-query" ON public.metric_rollup USING btree (metric, granularity, bucket_start DESC);

--
-- Name: idx-metric_rollup-unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-metric_rollup-unique" ON public.metric_rollup USING btree (metric, granularity, bucket_start, scope_hash);

--
-- Name: idx-phase_event-phase-at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-phase_event-phase-at" ON public.phase_event USING btree (phase, at);

--
-- Name: idx-phase_event-subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-phase_event-subject" ON public.phase_event USING btree (subject_kind, subject_id, at);

--
-- Name: idx-worker_connection-open; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-worker_connection-open" ON public.worker_connection USING btree (connected_at DESC) WHERE (disconnected_at IS NULL);

--
-- Name: idx-worker_connection-worker-connected_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-worker_connection-worker-connected_at" ON public.worker_connection USING btree (worker_id, connected_at DESC);

--
-- Name: idx-worker_sample-at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-worker_sample-at" ON public.worker_sample USING btree (at DESC);

--
-- Name: idx-worker_sample-worker-at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-worker_sample-worker-at" ON public.worker_sample USING btree (worker_id, at DESC);

--
-- Name: idx_api_cache; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_api_cache ON public.api USING btree (cache);

--
-- Name: idx_api_organization; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_api_organization ON public.api USING btree (organization);

--
-- Name: idx_api_owned_by; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_api_owned_by ON public.api USING btree (owned_by);

--
-- Name: idx_audit_log_user_id_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_user_id_created_at ON public.audit_log USING btree (user_id, created_at);

--
-- Name: idx_base_worker_worker_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_base_worker_worker_id ON public.base_worker USING btree (worker_id);

--
-- Name: idx_build_attempt_build; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_build_attempt_build ON public.build_attempt USING btree (build);

--
-- Name: idx_build_attempt_dispatched_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_build_attempt_dispatched_job ON public.build_attempt USING btree (dispatched_job);

--
-- Name: idx_build_log_chunk_build_index; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_build_log_chunk_build_index ON public.build_log_chunk USING btree (build, chunk_index);

--
-- Name: idx_cli_device_auth_expires_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_cli_device_auth_expires_at ON public.cli_device_authorization USING btree (expires_at);

--
-- Name: idx_org_base_worker_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_org_base_worker_unique ON public.organization_base_worker USING btree (organization, base_worker);

--
-- Name: idx_project_action_delivery_action_delivered; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_project_action_delivery_action_delivered ON public.project_action_delivery USING btree (action_id, delivered_at DESC);

--
-- Name: idx_project_action_project_name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_project_action_project_name ON public.project_action USING btree (project, name);

--
-- Name: idx_project_trigger_project_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_project_trigger_project_active ON public.project_trigger USING btree (project, active);

--
-- Name: idx_project_trigger_type_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_project_trigger_type_active ON public.project_trigger USING btree (trigger_type, active);

--
-- Name: idx_session_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_session_user_id ON public.session USING btree (user_id);

--
-- Name: idx_user_oidc_identity; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_user_oidc_identity ON public."user" USING btree (oidc_issuer, oidc_subject);

--
-- Name: idx_user_scim_external_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_user_scim_external_id ON public."user" USING btree (scim_external_id);

--
-- Name: idx_worker_registration_worker_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_worker_registration_worker_id ON public.worker_registration USING btree (worker_id);

--
-- Name: uq-open_pr_state-project-action-branch; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "uq-open_pr_state-project-action-branch" ON public.open_pr_state USING btree (project, action, branch);

--
-- Name: uq-project_flake_input_override-project-input_name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "uq-project_flake_input_override-project-input_name" ON public.project_flake_input_override USING btree (project, input_name);

--
-- Name: uq_evaluation_one_active_per_project; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uq_evaluation_one_active_per_project ON public.evaluation USING btree (project) WHERE ((project IS NOT NULL) AND (status = ANY (ARRAY[0, 1, 2, 3, 4, 8])) AND (NOT concurrent));

--
-- Name: ux-build_request_blob-org-hash; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "ux-build_request_blob-org-hash" ON public.build_request_blob USING btree (organization, hash);

--
-- Name: evaluation_flake_input_override evaluation_flake_input_override_evaluation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_flake_input_override
    ADD CONSTRAINT evaluation_flake_input_override_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: evaluation_input_update evaluation_input_update_evaluation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_input_update
    ADD CONSTRAINT evaluation_input_update_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: acknowledged_derivation fk-acknowledged_derivation-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.acknowledged_derivation
    ADD CONSTRAINT "fk-acknowledged_derivation-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;

--
-- Name: acknowledged_derivation fk-acknowledged_derivation-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.acknowledged_derivation
    ADD CONSTRAINT "fk-acknowledged_derivation-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: admin_task fk-admin_task-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.admin_task
    ADD CONSTRAINT "fk-admin_task-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;

--
-- Name: api fk-api-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.api
    ADD CONSTRAINT "fk-api-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: api fk-api-owned_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.api
    ADD CONSTRAINT "fk-api-owned_by" FOREIGN KEY (owned_by) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: audit_log fk-audit_log-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_log
    ADD CONSTRAINT "fk-audit_log-user" FOREIGN KEY (user_id) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE SET NULL;

--
-- Name: base_worker fk-base_worker-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.base_worker
    ADD CONSTRAINT "fk-base_worker-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;

--
-- Name: build fk-build-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build
    ADD CONSTRAINT "fk-build-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: build fk-build-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build
    ADD CONSTRAINT "fk-build-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: build fk-build-via; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build
    ADD CONSTRAINT "fk-build-via" FOREIGN KEY (via) REFERENCES public.build(id) ON DELETE SET NULL;

--
-- Name: build_product fk-build_product-derivation_output; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_product
    ADD CONSTRAINT "fk-build_product-derivation_output" FOREIGN KEY (derivation_output) REFERENCES public.derivation_output(id) ON DELETE CASCADE;

--
-- Name: build_request_blob fk-build_request_blob-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_request_blob
    ADD CONSTRAINT "fk-build_request_blob-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: cache fk-cache-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache
    ADD CONSTRAINT "fk-cache-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;

--
-- Name: cache_derivation fk-cache_derivation-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_derivation
    ADD CONSTRAINT "fk-cache_derivation-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;

--
-- Name: cache_derivation fk-cache_derivation-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_derivation
    ADD CONSTRAINT "fk-cache_derivation-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: cache_metric fk-cache_metric-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_metric
    ADD CONSTRAINT "fk-cache_metric-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;

--
-- Name: cache_role fk-cache_role-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_role
    ADD CONSTRAINT "fk-cache_role-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;

--
-- Name: cache_upstream fk-cache_upstream-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_upstream
    ADD CONSTRAINT "fk-cache_upstream-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: cache_user fk-cache_user-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT "fk-cache_user-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: cache_user fk-cache_user-role; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT "fk-cache_user-role" FOREIGN KEY (role) REFERENCES public.cache_role(id) ON UPDATE CASCADE ON DELETE RESTRICT;

--
-- Name: cache_user fk-cache_user-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT "fk-cache_user-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: cached_path_signature fk-cached_path_signature-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cached_path_signature
    ADD CONSTRAINT "fk-cached_path_signature-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;

--
-- Name: cached_path_signature fk-cached_path_signature-cached_path; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cached_path_signature
    ADD CONSTRAINT "fk-cached_path_signature-cached_path" FOREIGN KEY (cached_path) REFERENCES public.cached_path(id) ON DELETE CASCADE;

--
-- Name: cli_device_authorization fk-cli-device-auth-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cli_device_authorization
    ADD CONSTRAINT "fk-cli-device-auth-user" FOREIGN KEY (user_id) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: derivation fk-derivation-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation
    ADD CONSTRAINT "fk-derivation-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: derivation_closure fk-derivation_closure-dep; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_closure
    ADD CONSTRAINT "fk-derivation_closure-dep" FOREIGN KEY (dep_derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_closure fk-derivation_closure-root; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_closure
    ADD CONSTRAINT "fk-derivation_closure-root" FOREIGN KEY (root_derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_dependency fk-derivation_dependency-dependency; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_dependency
    ADD CONSTRAINT "fk-derivation_dependency-dependency" FOREIGN KEY (dependency) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_dependency fk-derivation_dependency-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_dependency
    ADD CONSTRAINT "fk-derivation_dependency-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_feature fk-derivation_feature-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_feature
    ADD CONSTRAINT "fk-derivation_feature-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_feature fk-derivation_feature-feature; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_feature
    ADD CONSTRAINT "fk-derivation_feature-feature" FOREIGN KEY (feature) REFERENCES public.system_requirement(id) ON DELETE CASCADE;

--
-- Name: derivation_metric fk-derivation_metric-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_metric
    ADD CONSTRAINT "fk-derivation_metric-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: derivation_output fk-derivation_output-cached_path; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_output
    ADD CONSTRAINT "fk-derivation_output-cached_path" FOREIGN KEY (cached_path) REFERENCES public.cached_path(id) ON DELETE SET NULL;

--
-- Name: derivation_output fk-derivation_output-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_output
    ADD CONSTRAINT "fk-derivation_output-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;

--
-- Name: entry_point fk-entry_point-build; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-build" FOREIGN KEY (build) REFERENCES public.build(id) ON DELETE CASCADE;

--
-- Name: entry_point fk-entry_point-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: entry_point fk-entry_point-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;

--
-- Name: entry_point_dep_count fk-entry_point_dep_count-entry_point; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point_dep_count
    ADD CONSTRAINT "fk-entry_point_dep_count-entry_point" FOREIGN KEY (entry_point) REFERENCES public.entry_point(id) ON DELETE CASCADE;

--
-- Name: entry_point_message fk-entry_point_message-entry_point; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point_message
    ADD CONSTRAINT "fk-entry_point_message-entry_point" FOREIGN KEY (entry_point) REFERENCES public.entry_point(id) ON DELETE CASCADE;

--
-- Name: entry_point_message fk-entry_point_message-message; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point_message
    ADD CONSTRAINT "fk-entry_point_message-message" FOREIGN KEY (message) REFERENCES public.evaluation_message(id) ON DELETE CASCADE;

--
-- Name: evaluation fk-evaluation-next; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-next" FOREIGN KEY (next) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: evaluation fk-evaluation-previous; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-previous" FOREIGN KEY (previous) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: evaluation fk-evaluation-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;

--
-- Name: evaluation fk-evaluation-started_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-started_by" FOREIGN KEY (started_by) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE SET NULL;

--
-- Name: evaluation fk-evaluation-trigger; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-trigger" FOREIGN KEY (trigger) REFERENCES public.project_trigger(id) ON UPDATE CASCADE ON DELETE SET NULL;

--
-- Name: evaluation_attr_cost fk-evaluation_attr_cost-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_attr_cost
    ADD CONSTRAINT "fk-evaluation_attr_cost-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: evaluation_message fk-evaluation_message-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_message
    ADD CONSTRAINT "fk-evaluation_message-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: evaluation_metric fk-evaluation_metric-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_metric
    ADD CONSTRAINT "fk-evaluation_metric-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: flake_output_node fk-flake_output_node-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flake_output_node
    ADD CONSTRAINT "fk-flake_output_node-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;

--
-- Name: integration fk-integration-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT "fk-integration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;

--
-- Name: integration fk-integration-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT "fk-integration-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: organization_base_worker fk-org_base_worker-base_worker; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_base_worker
    ADD CONSTRAINT "fk-org_base_worker-base_worker" FOREIGN KEY (base_worker) REFERENCES public.base_worker(id) ON DELETE CASCADE;

--
-- Name: organization_base_worker fk-org_base_worker-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_base_worker
    ADD CONSTRAINT "fk-org_base_worker-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;

--
-- Name: organization_base_worker fk-org_base_worker-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_base_worker
    ADD CONSTRAINT "fk-org_base_worker-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: organization fk-organization-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization
    ADD CONSTRAINT "fk-organization-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;

--
-- Name: organization_cache fk-organization_cache-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_cache
    ADD CONSTRAINT "fk-organization_cache-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: organization_cache fk-organization_cache-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_cache
    ADD CONSTRAINT "fk-organization_cache-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: organization_user fk-organization_user-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_user
    ADD CONSTRAINT "fk-organization_user-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: organization_user fk-organization_user-role; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_user
    ADD CONSTRAINT "fk-organization_user-role" FOREIGN KEY (role) REFERENCES public.role(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: organization_user fk-organization_user-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organization_user
    ADD CONSTRAINT "fk-organization_user-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: project fk-project-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT "fk-project-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;

--
-- Name: project fk-project-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT "fk-project-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: project_action fk-project_action-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_action
    ADD CONSTRAINT "fk-project_action-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE RESTRICT;

--
-- Name: project_action fk-project_action-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_action
    ADD CONSTRAINT "fk-project_action-project" FOREIGN KEY (project) REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: project_action_delivery fk-project_action_delivery-action_id; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_action_delivery
    ADD CONSTRAINT "fk-project_action_delivery-action_id" FOREIGN KEY (action_id) REFERENCES public.project_action(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: project_trigger fk-project_trigger-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_trigger
    ADD CONSTRAINT "fk-project_trigger-project" FOREIGN KEY (project) REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: role fk-role-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role
    ADD CONSTRAINT "fk-role-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: session fk-session-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session
    ADD CONSTRAINT "fk-session-user" FOREIGN KEY (user_id) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;

--
-- Name: upload_session fk-upload_session-organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upload_session
    ADD CONSTRAINT "fk-upload_session-organization" FOREIGN KEY (organization) REFERENCES public.organization(id) ON DELETE CASCADE;

--
-- Name: worker_registration fk-worker_registration-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.worker_registration
    ADD CONSTRAINT "fk-worker_registration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id);

--
-- Name: api fk_api_organization; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.api
    ADD CONSTRAINT fk_api_organization FOREIGN KEY (organization) REFERENCES public.organization(id) ON UPDATE CASCADE ON DELETE SET NULL;

--
-- Name: open_pr_state open_pr_state_action_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.open_pr_state
    ADD CONSTRAINT open_pr_state_action_fkey FOREIGN KEY (action) REFERENCES public.project_action(id) ON DELETE CASCADE;

--
-- Name: open_pr_state open_pr_state_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.open_pr_state
    ADD CONSTRAINT open_pr_state_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;

--
-- Name: project_flake_input_override project_flake_input_override_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_flake_input_override
    ADD CONSTRAINT project_flake_input_override_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;

--
-- PostgreSQL database dump complete
--


--
-- Global cache role seed required by fresh deployments (cache_user FK targets).
--

INSERT INTO public.cache_role (id, name, cache, permission, managed) VALUES
    ('00000000-0000-0000-0000-000000000011'::uuid, 'Admin', NULL, 1023, FALSE),
    ('00000000-0000-0000-0000-000000000012'::uuid, 'Write', NULL, 7, FALSE),
    ('00000000-0000-0000-0000-000000000013'::uuid, 'View',  NULL, 3, FALSE)
ON CONFLICT (id) DO NOTHING;
