--
-- PostgreSQL database dump
--


-- Dumped from database version 18.6
-- Dumped by pg_dump version 18.6


--
-- Name: evaluation_shared_build_counts(integer, boolean); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.evaluation_shared_build_counts(status integer, wanted boolean, OUT active integer, OUT failed integer, OUT queued integer, OUT building integer) RETURNS record
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT ((evaluation_shared_build_counts.status IN (0, 1, 2, 8, 10) AND (evaluation_shared_build_counts.wanted OR evaluation_shared_build_counts.status IN (1, 2))))::int, (evaluation_shared_build_counts.status IN (4, 5, 6, 9))::int, (evaluation_shared_build_counts.status = 1)::int, (evaluation_shared_build_counts.status = 2)::int $$;


--
-- Name: evaluation_shared_build_moved(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.evaluation_shared_build_moved() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT bj.evaluation, 0, nc.active - oc.active, nc.failed - oc.failed, nc.queued - oc.queued, nc.building - oc.building FROM evaluation_shared_build_counts(NEW.status, NEW.wanted) nc, evaluation_shared_build_counts(OLD.status, OLD.wanted) oc, build_job bj JOIN evaluation e ON e.id = bj.evaluation WHERE bj.derivation_build = NEW.id AND e.status IN (0, 8, 1, 2, 3, 4) AND (nc.active, nc.failed, nc.queued, nc.building) IS DISTINCT FROM (oc.active, oc.failed, oc.queued, oc.building); RETURN NULL; END $$;


--
-- Name: evaluation_shared_build_named(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.evaluation_shared_build_named() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT n.evaluation, count(*)::int, sum(c.active)::int, sum(c.failed)::int, sum(c.queued)::int, sum(c.building)::int FROM new_rows n JOIN evaluation e ON e.id = n.evaluation JOIN derivation_build db ON db.id = n.derivation_build CROSS JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) c WHERE e.status IN (0, 8, 1, 2, 3, 4) GROUP BY n.evaluation; RETURN NULL; END $$;


--
-- Name: evaluation_shared_build_unnamed(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.evaluation_shared_build_unnamed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN INSERT INTO evaluation_shared_build_delta (evaluation, named, active, failed, queued, building) SELECT o.evaluation, -count(*)::int, -coalesce(sum(c.active), 0)::int, -coalesce(sum(c.failed), 0)::int, -coalesce(sum(c.queued), 0)::int, -coalesce(sum(c.building), 0)::int FROM old_rows o JOIN evaluation e ON e.id = o.evaluation LEFT JOIN derivation_build db ON db.id = o.derivation_build LEFT JOIN LATERAL evaluation_shared_build_counts(db.status, db.wanted) c ON db.id IS NOT NULL WHERE e.status IN (0, 8, 1, 2, 3, 4) GROUP BY o.evaluation; RETURN NULL; END $$;


--
-- Name: ripple_missing_runtime_deps(uuid[], boolean); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.ripple_missing_runtime_deps(frontier uuid[], down boolean) RETURNS SETOF uuid
    LANGUAGE plpgsql
    AS $$
DECLARE
    wave uuid[] := frontier;
    parents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO parents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (1, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(parents) = 0;
        PERFORM pg_advisory_xact_lock(643, k)
           FROM (SELECT DISTINCT hashtext(a::text) AS k FROM unnest(parents) AS a ORDER BY k) keys;
        PERFORM 1 FROM derivation_build WHERE derivation = ANY(parents) ORDER BY derivation FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation_build db
               SET missing_runtime_deps = db.missing_runtime_deps + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(parents, counts) AS c(derivation, n)
             WHERE db.derivation = c.derivation
            RETURNING db.derivation,
                      CASE WHEN down THEN (db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)))
                           ELSE (db.missing_runtime_deps = c.n AND coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation), false))
                      END AS flipped)
        SELECT coalesce(array_agg(derivation ORDER BY derivation), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;


--
-- Name: ripple_unwalked_inputs(uuid[], boolean); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.ripple_unwalked_inputs(frontier uuid[], down boolean) RETURNS SETOF uuid
    LANGUAGE plpgsql
    AS $$
DECLARE
    wave uuid[] := frontier;
    parents uuid[];
    counts int[];
BEGIN
    WHILE cardinality(wave) > 0 LOOP
        SELECT coalesce(array_agg(x.derivation ORDER BY x.derivation), '{}'),
               coalesce(array_agg(x.n ORDER BY x.derivation), '{}')
          INTO parents, counts
          FROM (SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
                 WHERE e.dependency = ANY(wave) AND e.kind IN (0, 2)
                 GROUP BY e.derivation) x;
        EXIT WHEN cardinality(parents) = 0;
        PERFORM 1 FROM derivation WHERE id = ANY(parents) ORDER BY id FOR NO KEY UPDATE;
        WITH moved AS (
            UPDATE derivation d
               SET unwalked_inputs = d.unwalked_inputs + CASE WHEN down THEN -c.n ELSE c.n END
              FROM unnest(parents, counts) AS c(id, n)
             WHERE d.id = c.id
            RETURNING d.id, (d.walked AND d.unwalked_inputs = CASE WHEN down THEN 0 ELSE c.n END) AS flipped)
        SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO wave FROM moved WHERE flipped;
        RETURN QUERY SELECT unnest(wave);
    END LOOP;
END $$;


--
-- Name: unprioritize(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.unprioritize() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN NEW.prioritized := false; RETURN NEW; END $$;


SET default_tablespace = '';

SET default_table_access_method = heap;

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
    created_by uuid,
    checkpoint text
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
    project uuid,
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
-- Name: build_attempt; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_attempt (
    id uuid NOT NULL,
    build_job uuid,
    derivation_build uuid NOT NULL,
    dispatched_job uuid,
    substitute boolean DEFAULT false NOT NULL,
    outcome integer DEFAULT 0 NOT NULL,
    reason integer,
    failure_message text,
    build_context jsonb DEFAULT '{}'::jsonb NOT NULL,
    build_started_at timestamp without time zone,
    build_finished_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: build_job; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_job (
    id uuid NOT NULL,
    evaluation uuid NOT NULL,
    derivation uuid NOT NULL,
    derivation_build uuid NOT NULL,
    score double precision DEFAULT 0 NOT NULL,
    score_breakdown jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp without time zone NOT NULL,
    aborted boolean DEFAULT false NOT NULL
);
ALTER TABLE ONLY public.build_job ALTER COLUMN evaluation SET STATISTICS 10;


--
-- Name: build_log_chunk; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.build_log_chunk (
    id uuid NOT NULL,
    build_attempt uuid CONSTRAINT build_log_chunk_build_not_null NOT NULL,
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
    project uuid NOT NULL,
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
    max_storage_gb integer DEFAULT 0 NOT NULL,
    pull_through boolean DEFAULT true NOT NULL
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
-- Name: team_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_cache (
    id uuid NOT NULL,
    team uuid NOT NULL,
    cache uuid NOT NULL,
    role uuid NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: team_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_user (
    id uuid NOT NULL,
    team uuid NOT NULL,
    "user" uuid NOT NULL,
    role smallint NOT NULL,
    source smallint DEFAULT 0 NOT NULL
);


--
-- Name: cache_access; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.cache_access AS
 SELECT cu.cache,
    cu."user",
    cu.role,
    false AS via_team
   FROM public.cache_user cu
UNION ALL
 SELECT tc.cache,
    tu."user",
    tc.role,
    true AS via_team
   FROM (public.team_cache tc
     JOIN public.team_user tu ON ((tu.team = tc.team)));


--
-- Name: cache_invitation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_invitation (
    id uuid NOT NULL,
    cache uuid NOT NULL,
    "user" uuid NOT NULL,
    role uuid NOT NULL,
    invited_by uuid NOT NULL,
    token text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL
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
-- Name: cache_subscription_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_subscription_request (
    id uuid NOT NULL,
    project uuid NOT NULL,
    cache uuid NOT NULL,
    mode integer DEFAULT 0 NOT NULL,
    requested_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL
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
    api_key text,
    http1_only boolean DEFAULT false NOT NULL,
    active boolean DEFAULT true NOT NULL
);


--
-- Name: cache_usage; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cache_usage (
    cache uuid NOT NULL,
    bytes bigint NOT NULL
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
    ca text,
    created_at timestamp without time zone NOT NULL,
    deriver text,
    debug_info_indexed boolean DEFAULT false NOT NULL,
    confirmed boolean DEFAULT true NOT NULL,
    "references" text
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
-- Name: cluster_attempt; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cluster_attempt (
    id uuid NOT NULL,
    cluster_job uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    started_at timestamp without time zone,
    finished_at timestamp without time zone,
    outcome smallint
);


--
-- Name: cluster_job; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cluster_job (
    id uuid NOT NULL,
    status smallint DEFAULT 0 NOT NULL,
    same_zone boolean DEFAULT false NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    retry_budget integer NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);


--
-- Name: cluster_member; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.cluster_member (
    id uuid NOT NULL,
    cluster_job uuid NOT NULL,
    evaluation uuid,
    derivation_build uuid,
    role text NOT NULL,
    "primary" boolean DEFAULT false NOT NULL,
    pin text,
    CONSTRAINT cluster_member_check CHECK ((num_nonnulls(evaluation, derivation_build) = 1))
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
-- Name: debug_info; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.debug_info (
    id uuid NOT NULL,
    build_id text NOT NULL,
    cached_path uuid NOT NULL,
    member text NOT NULL,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: derivation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation (
    id uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    architecture text DEFAULT ''::text CONSTRAINT derivation_architecture_text_not_null NOT NULL,
    hash text NOT NULL,
    name text NOT NULL,
    pname character varying,
    prefer_local_build boolean DEFAULT false NOT NULL,
    allow_substitutes boolean DEFAULT true NOT NULL,
    closure_size bigint,
    is_fixed_output boolean DEFAULT false NOT NULL,
    walked boolean DEFAULT false NOT NULL,
    unwalked_inputs integer DEFAULT 0 NOT NULL,
    ifd boolean DEFAULT false NOT NULL
);


--
-- Name: derivation_build; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_build (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    status integer DEFAULT 0 NOT NULL,
    cache_available boolean DEFAULT false NOT NULL,
    substituted boolean DEFAULT false NOT NULL,
    attempt integer DEFAULT 0 NOT NULL,
    timeout_secs bigint,
    max_silent_secs bigint,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    queued_at timestamp without time zone,
    ready_at timestamp without time zone,
    dispatched_at timestamp without time zone,
    fetchable boolean DEFAULT false NOT NULL,
    blocking_deps integer DEFAULT 0 NOT NULL,
    wanted boolean DEFAULT true NOT NULL,
    missing_runtime_deps integer DEFAULT 0 NOT NULL,
    probed boolean DEFAULT true NOT NULL,
    prioritized boolean DEFAULT false NOT NULL
);


--
-- Name: derivation_dependency; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_dependency (
    derivation uuid NOT NULL,
    dependency uuid NOT NULL,
    kind smallint DEFAULT 0 NOT NULL
)
WITH (autovacuum_vacuum_scale_factor='0.02', autovacuum_analyze_scale_factor='0.02', autovacuum_vacuum_insert_scale_factor='0.02');


--
-- Name: derivation_feature; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_feature (
    id uuid NOT NULL,
    derivation uuid NOT NULL,
    feature uuid NOT NULL
);


--
-- Name: derivation_input_source; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.derivation_input_source (
    derivation uuid NOT NULL,
    hash text NOT NULL,
    store_path text NOT NULL,
    created_at timestamp without time zone NOT NULL
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
    architecture text NOT NULL,
    concurrent_builds integer,
    build_cores integer,
    cpu_core_score integer
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
    cached_path uuid,
    external_url text,
    nar_hash text,
    file_size bigint,
    references_list text,
    deriver text,
    file_hash text
);


--
-- Name: dispatched_job; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.dispatched_job (
    id uuid NOT NULL,
    kind smallint NOT NULL,
    evaluation_id uuid NOT NULL,
    project uuid NOT NULL,
    task uuid,
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
    instance_context jsonb,
    outcome smallint,
    job_id text,
    missing_nar_size bigint,
    missing_count integer,
    dependency_count integer,
    cluster_attempt uuid,
    worker_elapsed_ms bigint
);


--
-- Name: dispatched_job_phase; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.dispatched_job_phase (
    id uuid NOT NULL,
    dispatched_job uuid NOT NULL,
    seq integer NOT NULL,
    parent_seq integer,
    phase smallint NOT NULL,
    start_ms bigint DEFAULT 0 NOT NULL,
    end_ms bigint DEFAULT 0 NOT NULL,
    paths integer DEFAULT 0 NOT NULL,
    bytes bigint DEFAULT 0 NOT NULL,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: entry_point; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.entry_point (
    id uuid NOT NULL,
    task uuid NOT NULL,
    evaluation uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    eval character varying DEFAULT ''::character varying NOT NULL,
    repo_check_id bigint,
    derivation uuid NOT NULL,
    dep_counts_version bigint,
    dep_counts_computed_at timestamp without time zone
);
ALTER TABLE ONLY public.entry_point ALTER COLUMN evaluation SET STATISTICS 10;


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
    task uuid,
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
    kind integer DEFAULT 0 NOT NULL,
    graph_version bigint DEFAULT 0 NOT NULL,
    named_shared_builds integer DEFAULT 0 NOT NULL,
    active_shared_builds integer DEFAULT 0 NOT NULL,
    failed_shared_builds integer DEFAULT 0 NOT NULL,
    queued_shared_builds integer DEFAULT 0 NOT NULL,
    building_shared_builds integer DEFAULT 0 NOT NULL,
    walk_mode integer DEFAULT 0 NOT NULL,
    prioritized boolean DEFAULT false NOT NULL
)
WITH (fillfactor='50');


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
    updated_at timestamp without time zone NOT NULL,
    discover_only boolean DEFAULT false NOT NULL
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
-- Name: evaluation_shared_build_delta; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.evaluation_shared_build_delta (
    id bigint NOT NULL,
    evaluation uuid NOT NULL,
    named integer NOT NULL,
    active integer NOT NULL,
    failed integer NOT NULL,
    queued integer NOT NULL,
    building integer NOT NULL
);


--
-- Name: evaluation_shared_build_delta_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.evaluation_shared_build_delta_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: evaluation_shared_build_delta_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.evaluation_shared_build_delta_id_seq OWNED BY public.evaluation_shared_build_delta.id;


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
-- Name: github_installation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.github_installation (
    id uuid NOT NULL,
    project uuid NOT NULL,
    installation_id bigint NOT NULL,
    account_login text,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: integration; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.integration (
    id uuid NOT NULL,
    project uuid NOT NULL,
    name character varying NOT NULL,
    kind smallint NOT NULL,
    git_host_type smallint NOT NULL,
    secret text,
    endpoint_url text,
    access_token text,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    display_name character varying DEFAULT ''::character varying NOT NULL,
    allowed_ips text[],
    github_installation uuid,
    managed boolean DEFAULT false NOT NULL
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
    task uuid NOT NULL,
    action uuid NOT NULL,
    branch text NOT NULL,
    git_host_pr_number bigint,
    head_commit text,
    status text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);


--
-- Name: pending_delivery; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.pending_delivery (
    id uuid NOT NULL,
    kind smallint NOT NULL,
    key text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamp without time zone NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at timestamp without time zone NOT NULL,
    delivered_at timestamp without time zone,
    failed_at timestamp without time zone,
    last_error text
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
    name character varying NOT NULL,
    display_name character varying NOT NULL,
    description text NOT NULL,
    public_key character varying NOT NULL,
    private_key character varying NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    public boolean DEFAULT false NOT NULL,
    hide_build_requests boolean DEFAULT false NOT NULL
);


--
-- Name: project_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_user (
    id uuid NOT NULL,
    project uuid NOT NULL,
    "user" uuid NOT NULL,
    role uuid NOT NULL
);


--
-- Name: team_project; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_project (
    id uuid NOT NULL,
    team uuid NOT NULL,
    project uuid NOT NULL,
    role uuid,
    includes_users boolean NOT NULL,
    includes_workers boolean NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT team_project_check CHECK ((includes_users OR includes_workers)),
    CONSTRAINT team_project_check1 CHECK (((NOT includes_users) OR (role IS NOT NULL)))
);


--
-- Name: project_access; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.project_access AS
 SELECT pu.project,
    pu."user",
    pu.role,
    false AS via_team
   FROM public.project_user pu
UNION ALL
 SELECT tp.project,
    tu."user",
    tp.role,
    true AS via_team
   FROM (public.team_project tp
     JOIN public.team_user tu ON ((tu.team = tp.team)))
  WHERE tp.includes_users;


--
-- Name: project_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_cache (
    id uuid NOT NULL,
    project uuid NOT NULL,
    cache uuid NOT NULL,
    mode integer DEFAULT 0 NOT NULL
);


--
-- Name: project_invitation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.project_invitation (
    id uuid NOT NULL,
    project uuid NOT NULL,
    "user" uuid NOT NULL,
    role uuid NOT NULL,
    invited_by uuid NOT NULL,
    token text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL
);


--
-- Name: role; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.role (
    id uuid NOT NULL,
    name character varying NOT NULL,
    project uuid,
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
-- Name: storage_migration; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.storage_migration (
    name text NOT NULL,
    checkpoint text,
    started_at timestamp without time zone NOT NULL,
    applied_at timestamp without time zone
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
-- Name: task; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.task (
    id uuid NOT NULL,
    project uuid NOT NULL,
    name character varying NOT NULL,
    active boolean NOT NULL,
    display_name character varying NOT NULL,
    description text NOT NULL,
    repository character varying NOT NULL,
    wildcard character varying CONSTRAINT task_evaluation_wildcard_not_null NOT NULL,
    last_evaluation uuid,
    last_check_at timestamp without time zone NOT NULL,
    force_evaluation boolean NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    keep_evaluations integer DEFAULT 30 NOT NULL,
    concurrency smallint DEFAULT 1 NOT NULL,
    sign_cache boolean DEFAULT true NOT NULL,
    wait_for_workers boolean DEFAULT false NOT NULL,
    retry_failed_builds boolean DEFAULT true NOT NULL
);


--
-- Name: task_action; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.task_action (
    id uuid NOT NULL,
    task uuid NOT NULL,
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
-- Name: task_action_delivery; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.task_action_delivery (
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
-- Name: task_flake_input_override; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.task_flake_input_override (
    id uuid NOT NULL,
    task uuid NOT NULL,
    input_name text NOT NULL,
    url text,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);


--
-- Name: task_trigger; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.task_trigger (
    id uuid NOT NULL,
    task uuid NOT NULL,
    trigger_type smallint NOT NULL,
    config jsonb NOT NULL,
    active boolean DEFAULT true NOT NULL,
    last_fired_at timestamp without time zone,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL
);


--
-- Name: team; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team (
    id uuid NOT NULL,
    name character varying NOT NULL,
    display_name text NOT NULL,
    oidc_group text,
    scim_group text,
    new_project_users boolean DEFAULT false NOT NULL,
    new_project_workers boolean DEFAULT false NOT NULL,
    new_project_role uuid,
    created_by uuid,
    managed boolean DEFAULT false NOT NULL,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: team_cache_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_cache_request (
    id uuid NOT NULL,
    team uuid NOT NULL,
    cache uuid NOT NULL,
    role uuid NOT NULL,
    requested_by uuid,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: team_invitation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_invitation (
    id uuid NOT NULL,
    team uuid NOT NULL,
    "user" uuid NOT NULL,
    role smallint NOT NULL,
    invited_by uuid NOT NULL,
    token character varying NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL
);


--
-- Name: team_project_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_project_request (
    id uuid NOT NULL,
    team uuid NOT NULL,
    project uuid NOT NULL,
    role uuid,
    includes_users boolean NOT NULL,
    includes_workers boolean NOT NULL,
    requested_by uuid,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: team_worker; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_worker (
    id uuid NOT NULL,
    team uuid NOT NULL,
    worker_id character varying NOT NULL,
    token_hash character varying NOT NULL,
    token_encrypted text,
    url character varying,
    display_name text NOT NULL,
    gradient_ci boolean DEFAULT false NOT NULL,
    enable_fetch boolean DEFAULT true NOT NULL,
    enable_eval boolean DEFAULT true NOT NULL,
    enable_build boolean DEFAULT true NOT NULL,
    active boolean DEFAULT true NOT NULL,
    managed boolean DEFAULT false NOT NULL,
    created_by uuid,
    created_at timestamp without time zone NOT NULL
);


--
-- Name: upload_session; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.upload_session (
    id uuid NOT NULL,
    project uuid NOT NULL,
    manifest jsonb NOT NULL,
    missing jsonb NOT NULL,
    total_size bigint NOT NULL,
    created_at timestamp without time zone NOT NULL,
    expires_at timestamp without time zone NOT NULL,
    dispatched_at timestamp without time zone
);


--
-- Name: upstream_metric; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.upstream_metric (
    id uuid NOT NULL,
    bucket_time timestamp without time zone NOT NULL,
    latency_ms_sum double precision DEFAULT 0 NOT NULL,
    request_count integer DEFAULT 0 NOT NULL,
    narinfo_hits integer DEFAULT 0 NOT NULL,
    narinfo_misses integer DEFAULT 0 NOT NULL,
    upstream_url text NOT NULL
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
-- Name: user_cache_star; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_cache_star (
    "user" uuid NOT NULL,
    cache uuid NOT NULL,
    created_at timestamp without time zone DEFAULT now() NOT NULL
);


--
-- Name: user_project_star; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_project_star (
    "user" uuid NOT NULL,
    project uuid NOT NULL,
    created_at timestamp without time zone DEFAULT now() NOT NULL
);


--
-- Name: user_ssh_key; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_ssh_key (
    id uuid NOT NULL,
    "user" uuid NOT NULL,
    name text NOT NULL,
    public_key text NOT NULL,
    fingerprint text NOT NULL,
    created_at timestamp without time zone NOT NULL,
    last_used_at timestamp without time zone
);


--
-- Name: user_task_star; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_task_star (
    "user" uuid NOT NULL,
    task uuid NOT NULL,
    created_at timestamp without time zone DEFAULT now() NOT NULL
);


--
-- Name: webhook; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook (
    id uuid NOT NULL,
    scope smallint NOT NULL,
    project uuid,
    cache uuid,
    name text NOT NULL,
    url text NOT NULL,
    secret text NOT NULL,
    events jsonb DEFAULT '[]'::jsonb NOT NULL,
    active boolean DEFAULT true NOT NULL,
    created_by uuid NOT NULL,
    created_at timestamp without time zone NOT NULL,
    updated_at timestamp without time zone NOT NULL,
    last_fired_at timestamp without time zone,
    CONSTRAINT "chk-webhook-scope" CHECK ((((scope = 0) AND (project IS NOT NULL) AND (cache IS NULL)) OR ((scope = 1) AND (cache IS NOT NULL) AND (project IS NULL)) OR ((scope = 2) AND (project IS NULL) AND (cache IS NULL))))
);


--
-- Name: webhook_delivery; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_delivery (
    id uuid NOT NULL,
    webhook_id uuid NOT NULL,
    event text NOT NULL,
    request_body text NOT NULL,
    response_status integer,
    response_body text,
    error_message text,
    success boolean NOT NULL,
    duration_ms integer NOT NULL,
    delivered_at timestamp without time zone NOT NULL
);


--
-- Name: worker_connection; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.worker_connection (
    id uuid NOT NULL,
    worker_id character varying NOT NULL,
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
    enable_build boolean DEFAULT true NOT NULL,
    token_encrypted text,
    gradient_ci boolean DEFAULT false NOT NULL
);


--
-- Name: worker_sample; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.worker_sample (
    id uuid NOT NULL,
    worker_id character varying NOT NULL,
    at timestamp without time zone NOT NULL,
    cpu_usage_pct real,
    ram_free_mb bigint,
    ram_total_mb bigint,
    disk_speed_mbps real,
    assigned_jobs integer DEFAULT 0 NOT NULL,
    max_concurrent_builds integer DEFAULT 0 NOT NULL,
    state smallint DEFAULT 0 NOT NULL,
    capabilities jsonb NOT NULL,
    upload_speed_mbps real,
    download_speed_mbps real
);


--
-- Name: evaluation_shared_build_delta id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_shared_build_delta ALTER COLUMN id SET DEFAULT nextval('public.evaluation_shared_build_delta_id_seq'::regclass);


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
-- Name: build_attempt build_attempt_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_attempt
    ADD CONSTRAINT build_attempt_pkey PRIMARY KEY (id);


--
-- Name: build_job build_job_evaluation_derivation_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_job
    ADD CONSTRAINT build_job_evaluation_derivation_key UNIQUE (evaluation, derivation);


--
-- Name: build_job build_job_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_job
    ADD CONSTRAINT build_job_pkey PRIMARY KEY (id);


--
-- Name: build_log_chunk build_log_chunk_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_log_chunk
    ADD CONSTRAINT build_log_chunk_pkey PRIMARY KEY (id);


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
-- Name: cache_invitation cache_invitation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT cache_invitation_pkey PRIMARY KEY (id);


--
-- Name: cache_invitation cache_invitation_token_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT cache_invitation_token_key UNIQUE (token);


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
-- Name: cache_subscription_request cache_subscription_request_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_subscription_request
    ADD CONSTRAINT cache_subscription_request_pkey PRIMARY KEY (id);


--
-- Name: cache_upstream cache_upstream_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_upstream
    ADD CONSTRAINT cache_upstream_pkey PRIMARY KEY (id);


--
-- Name: cache_usage cache_usage_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_usage
    ADD CONSTRAINT cache_usage_pkey PRIMARY KEY (cache);


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
-- Name: cluster_attempt cluster_attempt_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_attempt
    ADD CONSTRAINT cluster_attempt_pkey PRIMARY KEY (id);


--
-- Name: cluster_job cluster_job_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_job
    ADD CONSTRAINT cluster_job_pkey PRIMARY KEY (id);


--
-- Name: cluster_member cluster_member_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_member
    ADD CONSTRAINT cluster_member_pkey PRIMARY KEY (id);


--
-- Name: commit commit_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.commit
    ADD CONSTRAINT commit_pkey PRIMARY KEY (id);


--
-- Name: debug_info debug_info_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.debug_info
    ADD CONSTRAINT debug_info_pkey PRIMARY KEY (id);


--
-- Name: derivation_build derivation_build_derivation_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_build
    ADD CONSTRAINT derivation_build_derivation_key UNIQUE (derivation);


--
-- Name: derivation_build derivation_build_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_build
    ADD CONSTRAINT derivation_build_pkey PRIMARY KEY (id);


--
-- Name: derivation_feature derivation_feature_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_feature
    ADD CONSTRAINT derivation_feature_pkey PRIMARY KEY (id);


--
-- Name: derivation_input_source derivation_input_source_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_input_source
    ADD CONSTRAINT derivation_input_source_pkey PRIMARY KEY (derivation, hash);


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
-- Name: dispatched_job_phase dispatched_job_phase_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dispatched_job_phase
    ADD CONSTRAINT dispatched_job_phase_pkey PRIMARY KEY (id);


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
-- Name: evaluation_shared_build_delta evaluation_shared_build_delta_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation_shared_build_delta
    ADD CONSTRAINT evaluation_shared_build_delta_pkey PRIMARY KEY (id);


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
-- Name: github_installation github_installation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT github_installation_pkey PRIMARY KEY (id);


--
-- Name: derivation_dependency idx-derivation_dependency-pair; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_dependency
    ADD CONSTRAINT "idx-derivation_dependency-pair" PRIMARY KEY (derivation, dependency);


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
-- Name: pending_delivery pending_delivery_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.pending_delivery
    ADD CONSTRAINT pending_delivery_pkey PRIMARY KEY (id);


--
-- Name: phase_event phase_event_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.phase_event
    ADD CONSTRAINT phase_event_pkey PRIMARY KEY (id);


--
-- Name: project_cache project_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_cache
    ADD CONSTRAINT project_cache_pkey PRIMARY KEY (id);


--
-- Name: project_invitation project_invitation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT project_invitation_pkey PRIMARY KEY (id);


--
-- Name: project_invitation project_invitation_token_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT project_invitation_token_key UNIQUE (token);


--
-- Name: project project_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT project_name_key UNIQUE (name);


--
-- Name: project project_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT project_pkey PRIMARY KEY (id);


--
-- Name: project_user project_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_user
    ADD CONSTRAINT project_user_pkey PRIMARY KEY (id);


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
-- Name: storage_migration storage_migration_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.storage_migration
    ADD CONSTRAINT storage_migration_pkey PRIMARY KEY (name);


--
-- Name: system_requirement system_requirement_name_kind_unique; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_requirement
    ADD CONSTRAINT system_requirement_name_kind_unique UNIQUE (name, kind);


--
-- Name: task_action_delivery task_action_delivery_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_action_delivery
    ADD CONSTRAINT task_action_delivery_pkey PRIMARY KEY (id);


--
-- Name: task_action task_action_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_action
    ADD CONSTRAINT task_action_pkey PRIMARY KEY (id);


--
-- Name: task_flake_input_override task_flake_input_override_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_flake_input_override
    ADD CONSTRAINT task_flake_input_override_pkey PRIMARY KEY (id);


--
-- Name: task task_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task
    ADD CONSTRAINT task_pkey PRIMARY KEY (id);


--
-- Name: task_trigger task_trigger_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_trigger
    ADD CONSTRAINT task_trigger_pkey PRIMARY KEY (id);


--
-- Name: team_cache team_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache
    ADD CONSTRAINT team_cache_pkey PRIMARY KEY (id);


--
-- Name: team_cache_request team_cache_request_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_pkey PRIMARY KEY (id);


--
-- Name: team_cache_request team_cache_request_team_cache_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_team_cache_key UNIQUE (team, cache);


--
-- Name: team_cache team_cache_team_cache_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache
    ADD CONSTRAINT team_cache_team_cache_key UNIQUE (team, cache);


--
-- Name: team_invitation team_invitation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_pkey PRIMARY KEY (id);


--
-- Name: team_invitation team_invitation_team_user_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_team_user_key UNIQUE (team, "user");


--
-- Name: team_invitation team_invitation_token_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_token_key UNIQUE (token);


--
-- Name: team team_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team
    ADD CONSTRAINT team_name_key UNIQUE (name);


--
-- Name: team team_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team
    ADD CONSTRAINT team_pkey PRIMARY KEY (id);


--
-- Name: team_project team_project_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project
    ADD CONSTRAINT team_project_pkey PRIMARY KEY (id);


--
-- Name: team_project_request team_project_request_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_pkey PRIMARY KEY (id);


--
-- Name: team_project_request team_project_request_team_project_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_team_project_key UNIQUE (team, project);


--
-- Name: team_project team_project_team_project_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project
    ADD CONSTRAINT team_project_team_project_key UNIQUE (team, project);


--
-- Name: team_user team_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_pkey PRIMARY KEY (id);


--
-- Name: team_user team_user_team_user_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_team_user_key UNIQUE (team, "user");


--
-- Name: team_worker team_worker_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_worker
    ADD CONSTRAINT team_worker_pkey PRIMARY KEY (id);


--
-- Name: team_worker team_worker_worker_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_worker
    ADD CONSTRAINT team_worker_worker_id_key UNIQUE (worker_id);


--
-- Name: upload_session upload_session_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upload_session
    ADD CONSTRAINT upload_session_pkey PRIMARY KEY (id);


--
-- Name: upstream_metric upstream_metric_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upstream_metric
    ADD CONSTRAINT upstream_metric_pkey PRIMARY KEY (id);


--
-- Name: cache_user uq-cache_user-cache-user; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_user
    ADD CONSTRAINT "uq-cache_user-cache-user" UNIQUE (cache, "user");


--
-- Name: user_cache_star user_cache_star_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_cache_star
    ADD CONSTRAINT user_cache_star_pkey PRIMARY KEY ("user", cache);


--
-- Name: user user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT user_pkey PRIMARY KEY (id);


--
-- Name: user_project_star user_project_star_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_project_star
    ADD CONSTRAINT user_project_star_pkey PRIMARY KEY ("user", project);


--
-- Name: user_ssh_key user_ssh_key_fingerprint_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_ssh_key
    ADD CONSTRAINT user_ssh_key_fingerprint_key UNIQUE (fingerprint);


--
-- Name: user_ssh_key user_ssh_key_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_ssh_key
    ADD CONSTRAINT user_ssh_key_pkey PRIMARY KEY (id);


--
-- Name: user_task_star user_task_star_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_task_star
    ADD CONSTRAINT user_task_star_pkey PRIMARY KEY ("user", task);


--
-- Name: user user_username_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT user_username_key UNIQUE (username);


--
-- Name: webhook_delivery webhook_delivery_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery
    ADD CONSTRAINT webhook_delivery_pkey PRIMARY KEY (id);


--
-- Name: webhook webhook_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT webhook_pkey PRIMARY KEY (id);


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
-- Name: idx-admin_task-kind-finished_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-admin_task-kind-finished_at" ON public.admin_task USING btree (kind, finished_at) WHERE (finished_at IS NOT NULL);


--
-- Name: idx-audit_log-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-audit_log-created_at" ON public.audit_log USING btree (created_at);


--
-- Name: idx-build_attempt-build_finished_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_attempt-build_finished_at" ON public.build_attempt USING btree (build_finished_at) WHERE (build_finished_at IS NOT NULL);


--
-- Name: idx-build_attempt-build_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_attempt-build_job" ON public.build_attempt USING btree (build_job) WHERE (build_job IS NOT NULL);


--
-- Name: idx-build_attempt-deterministic-failure; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_attempt-deterministic-failure" ON public.build_attempt USING btree (derivation_build) WHERE ((outcome = 3) AND (reason = 5));


--
-- Name: idx-build_attempt-latest; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_attempt-latest" ON public.build_attempt USING btree (derivation_build, created_at DESC);


--
-- Name: idx-build_job-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_job-created_at" ON public.build_job USING btree (created_at) INCLUDE (derivation);


--
-- Name: idx-build_job-derivation-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_job-derivation-evaluation" ON public.build_job USING btree (derivation) INCLUDE (evaluation);


--
-- Name: idx-build_job-evaluation-shared_build; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_job-evaluation-shared_build" ON public.build_job USING btree (evaluation) INCLUDE (derivation_build);


--
-- Name: idx-build_product-derivation_output; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-build_product-derivation_output" ON public.build_product USING btree (derivation_output);


--
-- Name: idx-cache_invitation-cache-user; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cache_invitation-cache-user" ON public.cache_invitation USING btree (cache, "user");


--
-- Name: idx-cache_metric-bucket_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cache_metric-bucket_time" ON public.cache_metric USING btree (bucket_time);


--
-- Name: idx-cache_metric-cache-bucket_time; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cache_metric-cache-bucket_time" ON public.cache_metric USING btree (cache, bucket_time);


--
-- Name: idx-cache_subscription_request-project-cache; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cache_subscription_request-project-cache" ON public.cache_subscription_request USING btree (project, cache);


--
-- Name: idx-cached_path-debug_info_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path-debug_info_pending" ON public.cached_path USING btree (created_at) WHERE ((NOT debug_info_indexed) AND (package ~~ '%-debug'::text));


--
-- Name: idx-cached_path-references; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path-references" ON public.cached_path USING gin (string_to_array("references", ' '::text)) WITH (fastupdate=off);


--
-- Name: idx-cached_path-unconfirmed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path-unconfirmed" ON public.cached_path USING btree (created_at) WHERE (NOT confirmed);


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
-- Name: idx-cached_path_signature-pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cached_path_signature-pending" ON public.cached_path_signature USING btree (id) WHERE (signature IS NULL);


--
-- Name: idx-cluster_attempt-cluster_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cluster_attempt-cluster_job" ON public.cluster_attempt USING btree (cluster_job);


--
-- Name: idx-cluster_attempt-open; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cluster_attempt-open" ON public.cluster_attempt USING btree (cluster_job) WHERE (finished_at IS NULL);


--
-- Name: idx-cluster_job-finished-updated_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cluster_job-finished-updated_at" ON public.cluster_job USING btree (updated_at) WHERE (status = ANY (ARRAY[2, 3, 4]));


--
-- Name: idx-cluster_member-cluster_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-cluster_member-cluster_job" ON public.cluster_member USING btree (cluster_job);


--
-- Name: idx-cluster_member-derivation_build; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cluster_member-derivation_build" ON public.cluster_member USING btree (derivation_build) WHERE (derivation_build IS NOT NULL);


--
-- Name: idx-cluster_member-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-cluster_member-evaluation" ON public.cluster_member USING btree (evaluation) WHERE (evaluation IS NOT NULL);


--
-- Name: idx-commit-hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-commit-hash" ON public.commit USING btree (hash);


--
-- Name: idx-debug_info-build_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-debug_info-build_id" ON public.debug_info USING btree (build_id);


--
-- Name: idx-debug_info-cached_path; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-debug_info-cached_path" ON public.debug_info USING btree (cached_path);


--
-- Name: idx-debug_info-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-debug_info-pair" ON public.debug_info USING btree (build_id, cached_path);


--
-- Name: idx-derivation-hash-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation-hash-name" ON public.derivation USING btree (hash, name);


--
-- Name: idx-derivation_build-active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-active" ON public.derivation_build USING btree (derivation) WHERE (status = ANY (ARRAY[0, 1, 2, 8]));


--
-- Name: idx-derivation_build-completed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-completed" ON public.derivation_build USING btree (created_at) WHERE (status = 3);


--
-- Name: idx-derivation_build-dispatch-startable; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-dispatch-startable" ON public.derivation_build USING btree (updated_at) WHERE (status = 1);


--
-- Name: idx-derivation_build-fetchable-incomplete; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-fetchable-incomplete" ON public.derivation_build USING btree (derivation) WHERE (fetchable AND (missing_runtime_deps > 0));


--
-- Name: idx-derivation_build-open; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-open" ON public.derivation_build USING btree (derivation) WHERE ((NOT fetchable) AND (status <> ALL (ARRAY[4, 6, 9])));


--
-- Name: idx-derivation_build-promotable; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-promotable" ON public.derivation_build USING btree (derivation) WHERE ((status = 0) AND ((blocking_deps = 0) OR cache_available));


--
-- Name: idx-derivation_build-terminal-failed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-terminal-failed" ON public.derivation_build USING btree (derivation) WHERE (status = ANY (ARRAY[4, 5, 6, 9]));


--
-- Name: idx-derivation_build-unprobed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-unprobed" ON public.derivation_build USING btree (derivation) WHERE ((NOT probed) AND wanted);


--
-- Name: idx-derivation_build-unsettled; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_build-unsettled" ON public.derivation_build USING btree (derivation) WHERE (status <> ALL (ARRAY[3, 7]));


--
-- Name: idx-derivation_dependency-reverse-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_dependency-reverse-pair" ON public.derivation_dependency USING btree (dependency, derivation);


--
-- Name: idx-derivation_dependency-runtime; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_dependency-runtime" ON public.derivation_dependency USING btree (dependency) INCLUDE (derivation) WHERE (kind = ANY (ARRAY[1, 2]));


--
-- Name: idx-derivation_feature-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-derivation_feature-pair" ON public.derivation_feature USING btree (derivation, feature);


--
-- Name: idx-derivation_metric-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_metric-created_at" ON public.derivation_metric USING btree (created_at);


--
-- Name: idx-derivation_metric-derivation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_metric-derivation" ON public.derivation_metric USING btree (derivation);


--
-- Name: idx-derivation_metric-pname-architecture-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_metric-pname-architecture-created_at" ON public.derivation_metric USING btree (pname, architecture, created_at DESC);


--
-- Name: idx-derivation_output-cached_path; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_output-cached_path" ON public.derivation_output USING btree (cached_path) WHERE (cached_path IS NOT NULL);


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
-- Name: idx-derivation_output-references_list; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-derivation_output-references_list" ON public.derivation_output USING gin (string_to_array(references_list, ' '::text)) WITH (fastupdate=off);


--
-- Name: idx-dispatched_job-build-window; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-build-window" ON public.dispatched_job USING btree (dispatched_at) INCLUDE (ready_at, missing_nar_size, missing_count, dependency_count) WHERE ((kind = 1) AND (ready_at IS NOT NULL));


--
-- Name: idx-dispatched_job-cluster_attempt; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-cluster_attempt" ON public.dispatched_job USING btree (cluster_attempt) WHERE (cluster_attempt IS NOT NULL);


--
-- Name: idx-dispatched_job-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-created_at" ON public.dispatched_job USING btree (created_at);


--
-- Name: idx-dispatched_job-eval-by-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-eval-by-evaluation" ON public.dispatched_job USING btree (evaluation_id, dispatched_at DESC) WHERE (kind = 0);


--
-- Name: idx-dispatched_job-open; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-open" ON public.dispatched_job USING btree (dispatched_at DESC) WHERE (finished_at IS NULL);


--
-- Name: idx-dispatched_job-open-by-worker; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-open-by-worker" ON public.dispatched_job USING btree (worker_id, evaluation_id, dispatched_at DESC) WHERE (finished_at IS NULL);


--
-- Name: idx-dispatched_job-open-job; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-dispatched_job-open-job" ON public.dispatched_job USING btree (job_id) WHERE (finished_at IS NULL);


--
-- Name: idx-dispatched_job-project-dispatched_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-project-dispatched_at" ON public.dispatched_job USING btree (project, dispatched_at DESC);


--
-- Name: idx-dispatched_job-worker-dispatched_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job-worker-dispatched_at" ON public.dispatched_job USING btree (worker_id, dispatched_at DESC);


--
-- Name: idx-dispatched_job_phase-job-seq; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-dispatched_job_phase-job-seq" ON public.dispatched_job_phase USING btree (dispatched_job, seq);


--
-- Name: idx-dispatched_job_phase-phase-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-dispatched_job_phase-phase-created_at" ON public.dispatched_job_phase USING btree (phase, created_at);


--
-- Name: idx-entry_point-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-entry_point-created_at" ON public.entry_point USING btree (created_at) INCLUDE (derivation);


--
-- Name: idx-entry_point-derivation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-entry_point-derivation" ON public.entry_point USING btree (derivation);


--
-- Name: idx-entry_point-evaluation-eval; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-entry_point-evaluation-eval" ON public.entry_point USING btree (evaluation, eval);


--
-- Name: idx-entry_point_dep_count-pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-entry_point_dep_count-pair" ON public.entry_point_dep_count USING btree (entry_point, status);


--
-- Name: idx-entry_point_message-message; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-entry_point_message-message" ON public.entry_point_message USING btree (message);


--
-- Name: idx-entry_point_message-unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-entry_point_message-unique" ON public.entry_point_message USING btree (entry_point, message);


--
-- Name: idx-eval_cache_store-fingerprint; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-eval_cache_store-fingerprint" ON public.eval_cache_store USING btree (fingerprint);


--
-- Name: idx-evaluation-commit; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation-commit" ON public.evaluation USING btree (commit);


--
-- Name: idx-evaluation-next; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation-next" ON public.evaluation USING btree (next) WHERE (next IS NOT NULL);


--
-- Name: idx-evaluation-previous; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation-previous" ON public.evaluation USING btree (previous) WHERE (previous IS NOT NULL);


--
-- Name: idx-evaluation-task-created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation-task-created_at" ON public.evaluation USING btree (task, created_at DESC);


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
-- Name: idx-evaluation_shared_build_delta-evaluation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-evaluation_shared_build_delta-evaluation" ON public.evaluation_shared_build_delta USING btree (evaluation);


--
-- Name: idx-flake_output_node-evaluation-parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-flake_output_node-evaluation-parent" ON public.flake_output_node USING btree (evaluation, parent);


--
-- Name: idx-github-installation-project-installation; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-github-installation-project-installation" ON public.github_installation USING btree (project, installation_id);


--
-- Name: idx-integration-project-kind-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-integration-project-kind-name" ON public.integration USING btree (project, kind, name);


--
-- Name: idx-metric_rollup-query; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-metric_rollup-query" ON public.metric_rollup USING btree (metric, granularity, bucket_start DESC);


--
-- Name: idx-metric_rollup-unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-metric_rollup-unique" ON public.metric_rollup USING btree (metric, granularity, bucket_start, scope_hash);


--
-- Name: idx-pending_delivery-due; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-pending_delivery-due" ON public.pending_delivery USING btree (next_attempt_at) WHERE ((delivered_at IS NULL) AND (failed_at IS NULL));


--
-- Name: idx-pending_delivery-key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-pending_delivery-key" ON public.pending_delivery USING btree (kind, key) WHERE ((delivered_at IS NULL) AND (failed_at IS NULL));


--
-- Name: idx-pending_delivery-settled; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-pending_delivery-settled" ON public.pending_delivery USING btree (COALESCE(delivered_at, failed_at)) WHERE ((delivered_at IS NOT NULL) OR (failed_at IS NOT NULL));


--
-- Name: idx-phase_event-phase-at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-phase_event-phase-at" ON public.phase_event USING btree (phase, at);


--
-- Name: idx-phase_event-subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-phase_event-subject" ON public.phase_event USING btree (subject_kind, subject_id, at);


--
-- Name: idx-project_invitation-project-user; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-project_invitation-project-user" ON public.project_invitation USING btree (project, "user");


--
-- Name: idx-session-expires_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-session-expires_at" ON public.session USING btree (expires_at);


--
-- Name: idx-task_action_delivery-delivered_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-task_action_delivery-delivered_at" ON public.task_action_delivery USING btree (delivered_at);


--
-- Name: idx-upstream_metric-bucket_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-upstream_metric-bucket_time" ON public.upstream_metric USING btree (bucket_time);


--
-- Name: idx-upstream_metric-upstream_url-bucket_time; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-upstream_metric-upstream_url-bucket_time" ON public.upstream_metric USING btree (upstream_url, bucket_time);


--
-- Name: idx-user_cache_star-cache; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-user_cache_star-cache" ON public.user_cache_star USING btree (cache);


--
-- Name: idx-user_project_star-project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-user_project_star-project" ON public.user_project_star USING btree (project);


--
-- Name: idx-user_ssh_key-user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-user_ssh_key-user" ON public.user_ssh_key USING btree ("user");


--
-- Name: idx-user_task_star-task; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-user_task_star-task" ON public.user_task_star USING btree (task);


--
-- Name: idx-webhook-cache; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-webhook-cache" ON public.webhook USING btree (cache) WHERE (active AND (cache IS NOT NULL));


--
-- Name: idx-webhook-delivery-webhook; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-webhook-delivery-webhook" ON public.webhook_delivery USING btree (webhook_id, delivered_at DESC);


--
-- Name: idx-webhook-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "idx-webhook-name" ON public.webhook USING btree (scope, COALESCE(project, cache), name);


--
-- Name: idx-webhook-project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-webhook-project" ON public.webhook USING btree (project) WHERE (active AND (project IS NOT NULL));


--
-- Name: idx-webhook_delivery-delivered_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-webhook_delivery-delivered_at" ON public.webhook_delivery USING btree (delivered_at);


--
-- Name: idx-worker_connection-disconnected_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "idx-worker_connection-disconnected_at" ON public.worker_connection USING btree (disconnected_at) WHERE (disconnected_at IS NOT NULL);


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
-- Name: idx_api_owned_by; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_api_owned_by ON public.api USING btree (owned_by);


--
-- Name: idx_api_owned_by_name_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_api_owned_by_name_unique ON public.api USING btree (owned_by, name);


--
-- Name: idx_api_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_api_project ON public.api USING btree (project);


--
-- Name: idx_audit_log_user_id_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_log_user_id_created_at ON public.audit_log USING btree (user_id, created_at);


--
-- Name: idx_build_attempt_derivation_build; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_build_attempt_derivation_build ON public.build_attempt USING btree (derivation_build);


--
-- Name: idx_build_attempt_dispatched_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_build_attempt_dispatched_job ON public.build_attempt USING btree (dispatched_job);


--
-- Name: idx_build_job_derivation_build; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_build_job_derivation_build ON public.build_job USING btree (derivation_build);


--
-- Name: idx_build_log_chunk_build_index; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_build_log_chunk_build_index ON public.build_log_chunk USING btree (build_attempt, chunk_index);


--
-- Name: idx_cache_role_cache_name_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_cache_role_cache_name_unique ON public.cache_role USING btree (cache, name) WHERE (cache IS NOT NULL);


--
-- Name: idx_cli_device_auth_expires_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_cli_device_auth_expires_at ON public.cli_device_authorization USING btree (expires_at);


--
-- Name: idx_derivation_input_source_hash; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_derivation_input_source_hash ON public.derivation_input_source USING btree (hash);


--
-- Name: idx_project_cache_project_cache_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_project_cache_project_cache_unique ON public.project_cache USING btree (project, cache);


--
-- Name: idx_project_user_project_user_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_project_user_project_user_unique ON public.project_user USING btree (project, "user");


--
-- Name: idx_role_project_name_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_role_project_name_unique ON public.role USING btree (project, name) WHERE (project IS NOT NULL);


--
-- Name: idx_session_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_session_user_id ON public.session USING btree (user_id);


--
-- Name: idx_task_action_delivery_action_delivered; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_task_action_delivery_action_delivered ON public.task_action_delivery USING btree (action_id, delivered_at DESC);


--
-- Name: idx_task_action_task_name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_task_action_task_name ON public.task_action USING btree (task, name);


--
-- Name: idx_task_trigger_task_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_task_trigger_task_active ON public.task_trigger USING btree (task, active);


--
-- Name: idx_task_trigger_type_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_task_trigger_type_active ON public.task_trigger USING btree (trigger_type, active);


--
-- Name: idx_team_cache_cache; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_cache_cache ON public.team_cache USING btree (cache);


--
-- Name: idx_team_oidc_group; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_oidc_group ON public.team USING btree (oidc_group) WHERE (oidc_group IS NOT NULL);


--
-- Name: idx_team_project_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_project_project ON public.team_project USING btree (project);


--
-- Name: idx_team_scim_group; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_team_scim_group ON public.team USING btree (scim_group) WHERE (scim_group IS NOT NULL);


--
-- Name: idx_team_user_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_user_user ON public.team_user USING btree ("user");


--
-- Name: idx_team_worker_one_gradient_ci; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_team_worker_one_gradient_ci ON public.team_worker USING btree (team) WHERE gradient_ci;


--
-- Name: idx_team_worker_team; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_team_worker_team ON public.team_worker USING btree (team);


--
-- Name: idx_user_oidc_identity; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_user_oidc_identity ON public."user" USING btree (oidc_issuer, oidc_subject);


--
-- Name: idx_user_scim_external_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_user_scim_external_id ON public."user" USING btree (scim_external_id);


--
-- Name: idx_worker_registration_one_gradient_ci; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_worker_registration_one_gradient_ci ON public.worker_registration USING btree (peer_id) WHERE gradient_ci;


--
-- Name: idx_worker_registration_peer_id_worker_id_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_worker_registration_peer_id_worker_id_unique ON public.worker_registration USING btree (peer_id, worker_id);


--
-- Name: idx_worker_registration_worker_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_worker_registration_worker_id ON public.worker_registration USING btree (worker_id);


--
-- Name: uq-open_pr_state-task-action-branch; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "uq-open_pr_state-task-action-branch" ON public.open_pr_state USING btree (task, action, branch);


--
-- Name: uq-task-project-name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "uq-task-project-name" ON public.task USING btree (project, name);


--
-- Name: uq-task_flake_input_override-task-input_name; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "uq-task_flake_input_override-task-input_name" ON public.task_flake_input_override USING btree (task, input_name);


--
-- Name: uq_evaluation_one_active_per_task; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uq_evaluation_one_active_per_task ON public.evaluation USING btree (task) WHERE ((task IS NOT NULL) AND (status = ANY (ARRAY[0, 1, 2, 3, 4, 8])) AND (NOT concurrent));


--
-- Name: ux-build_request_blob-project-hash; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "ux-build_request_blob-project-hash" ON public.build_request_blob USING btree (project, hash);


--
-- Name: stat-derivation_build-can_start; Type: STATISTICS; Schema: public; Owner: -
--

CREATE STATISTICS public."stat-derivation_build-can_start" ON status, fetchable, blocking_deps FROM public.derivation_build;


--
-- Name: derivation_build derivation_build_unprioritize; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER derivation_build_unprioritize BEFORE UPDATE OF status ON public.derivation_build FOR EACH ROW WHEN ((new.prioritized AND (new.status = ANY (ARRAY[4, 5, 6, 9, 10])))) EXECUTE FUNCTION public.unprioritize();


--
-- Name: derivation_build evaluation_shared_build_moved; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER evaluation_shared_build_moved AFTER UPDATE OF status, wanted ON public.derivation_build FOR EACH ROW WHEN (((old.status IS DISTINCT FROM new.status) OR (old.wanted IS DISTINCT FROM new.wanted))) EXECUTE FUNCTION public.evaluation_shared_build_moved();


--
-- Name: build_job evaluation_shared_build_named; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER evaluation_shared_build_named AFTER INSERT ON public.build_job REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION public.evaluation_shared_build_named();


--
-- Name: build_job evaluation_shared_build_unnamed; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER evaluation_shared_build_unnamed AFTER DELETE ON public.build_job REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION public.evaluation_shared_build_unnamed();


--
-- Name: evaluation evaluation_unprioritize; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER evaluation_unprioritize BEFORE UPDATE OF status ON public.evaluation FOR EACH ROW WHEN ((new.prioritized AND (new.status = ANY (ARRAY[6, 7])))) EXECUTE FUNCTION public.unprioritize();


--
-- Name: build_attempt build_attempt_build_job_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_attempt
    ADD CONSTRAINT build_attempt_build_job_fkey FOREIGN KEY (build_job) REFERENCES public.build_job(id) ON DELETE SET NULL;


--
-- Name: build_attempt build_attempt_derivation_build_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_attempt
    ADD CONSTRAINT build_attempt_derivation_build_fkey FOREIGN KEY (derivation_build) REFERENCES public.derivation_build(id) ON DELETE CASCADE;


--
-- Name: build_job build_job_derivation_build_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_job
    ADD CONSTRAINT build_job_derivation_build_fkey FOREIGN KEY (derivation_build) REFERENCES public.derivation_build(id) ON DELETE CASCADE;


--
-- Name: build_job build_job_derivation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_job
    ADD CONSTRAINT build_job_derivation_fkey FOREIGN KEY (derivation) REFERENCES public.derivation(id);


--
-- Name: build_job build_job_evaluation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_job
    ADD CONSTRAINT build_job_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;


--
-- Name: cache_usage cache_usage_cache_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_usage
    ADD CONSTRAINT cache_usage_cache_fkey FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: cluster_attempt cluster_attempt_cluster_job_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_attempt
    ADD CONSTRAINT cluster_attempt_cluster_job_fkey FOREIGN KEY (cluster_job) REFERENCES public.cluster_job(id) ON DELETE CASCADE;


--
-- Name: cluster_member cluster_member_cluster_job_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_member
    ADD CONSTRAINT cluster_member_cluster_job_fkey FOREIGN KEY (cluster_job) REFERENCES public.cluster_job(id) ON DELETE CASCADE;


--
-- Name: cluster_member cluster_member_derivation_build_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_member
    ADD CONSTRAINT cluster_member_derivation_build_fkey FOREIGN KEY (derivation_build) REFERENCES public.derivation_build(id) ON DELETE CASCADE;


--
-- Name: cluster_member cluster_member_evaluation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cluster_member
    ADD CONSTRAINT cluster_member_evaluation_fkey FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;


--
-- Name: debug_info debug_info_cached_path_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.debug_info
    ADD CONSTRAINT debug_info_cached_path_fkey FOREIGN KEY (cached_path) REFERENCES public.cached_path(id) ON DELETE CASCADE;


--
-- Name: derivation_build derivation_build_derivation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_build
    ADD CONSTRAINT derivation_build_derivation_fkey FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;


--
-- Name: derivation_input_source derivation_input_source_derivation_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.derivation_input_source
    ADD CONSTRAINT derivation_input_source_derivation_fkey FOREIGN KEY (derivation) REFERENCES public.derivation(id) ON DELETE CASCADE;


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
-- Name: build_log_chunk fk-build_log_chunk-build_attempt; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_log_chunk
    ADD CONSTRAINT "fk-build_log_chunk-build_attempt" FOREIGN KEY (build_attempt) REFERENCES public.build_attempt(id) ON DELETE CASCADE;


--
-- Name: build_product fk-build_product-derivation_output; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_product
    ADD CONSTRAINT "fk-build_product-derivation_output" FOREIGN KEY (derivation_output) REFERENCES public.derivation_output(id) ON DELETE CASCADE;


--
-- Name: build_request_blob fk-build_request_blob-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.build_request_blob
    ADD CONSTRAINT "fk-build_request_blob-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: cache fk-cache-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache
    ADD CONSTRAINT "fk-cache-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: cache_invitation fk-cache_invitation-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT "fk-cache_invitation-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: cache_invitation fk-cache_invitation-invited_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT "fk-cache_invitation-invited_by" FOREIGN KEY (invited_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: cache_invitation fk-cache_invitation-role; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT "fk-cache_invitation-role" FOREIGN KEY (role) REFERENCES public.cache_role(id) ON DELETE CASCADE;


--
-- Name: cache_invitation fk-cache_invitation-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_invitation
    ADD CONSTRAINT "fk-cache_invitation-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


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
-- Name: cache_subscription_request fk-cache_subscription_request-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_subscription_request
    ADD CONSTRAINT "fk-cache_subscription_request-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: cache_subscription_request fk-cache_subscription_request-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_subscription_request
    ADD CONSTRAINT "fk-cache_subscription_request-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: cache_subscription_request fk-cache_subscription_request-requested_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.cache_subscription_request
    ADD CONSTRAINT "fk-cache_subscription_request-requested_by" FOREIGN KEY (requested_by) REFERENCES public."user"(id) ON DELETE CASCADE;


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
-- Name: dispatched_job fk-dispatched_job-cluster_attempt; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dispatched_job
    ADD CONSTRAINT "fk-dispatched_job-cluster_attempt" FOREIGN KEY (cluster_attempt) REFERENCES public.cluster_attempt(id) ON DELETE SET NULL;


--
-- Name: dispatched_job_phase fk-dispatched_job_phase-dispatched_job; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.dispatched_job_phase
    ADD CONSTRAINT "fk-dispatched_job_phase-dispatched_job" FOREIGN KEY (dispatched_job) REFERENCES public.dispatched_job(id) ON DELETE CASCADE;


--
-- Name: entry_point fk-entry_point-derivation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-derivation" FOREIGN KEY (derivation) REFERENCES public.derivation(id);


--
-- Name: entry_point fk-entry_point-evaluation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-evaluation" FOREIGN KEY (evaluation) REFERENCES public.evaluation(id) ON DELETE CASCADE;


--
-- Name: entry_point fk-entry_point-task; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entry_point
    ADD CONSTRAINT "fk-entry_point-task" FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE;


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
-- Name: evaluation fk-evaluation-started_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-started_by" FOREIGN KEY (started_by) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE SET NULL;


--
-- Name: evaluation fk-evaluation-task; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-task" FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE;


--
-- Name: evaluation fk-evaluation-trigger; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.evaluation
    ADD CONSTRAINT "fk-evaluation-trigger" FOREIGN KEY (trigger) REFERENCES public.task_trigger(id) ON UPDATE CASCADE ON DELETE SET NULL;


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
-- Name: github_installation fk-github_installation-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT "fk-github_installation-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id);


--
-- Name: github_installation fk-github_installation-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.github_installation
    ADD CONSTRAINT "fk-github_installation-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: integration fk-integration-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT "fk-integration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: integration fk-integration-github-installation; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT "fk-integration-github-installation" FOREIGN KEY (github_installation) REFERENCES public.github_installation(id) ON DELETE SET NULL;


--
-- Name: integration fk-integration-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.integration
    ADD CONSTRAINT "fk-integration-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: project fk-project-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project
    ADD CONSTRAINT "fk-project-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: project_cache fk-project_cache-cache; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_cache
    ADD CONSTRAINT "fk-project_cache-cache" FOREIGN KEY (cache) REFERENCES public.cache(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: project_cache fk-project_cache-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_cache
    ADD CONSTRAINT "fk-project_cache-project" FOREIGN KEY (project) REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: project_invitation fk-project_invitation-invited_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT "fk-project_invitation-invited_by" FOREIGN KEY (invited_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: project_invitation fk-project_invitation-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT "fk-project_invitation-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: project_invitation fk-project_invitation-role; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT "fk-project_invitation-role" FOREIGN KEY (role) REFERENCES public.role(id) ON DELETE CASCADE;


--
-- Name: project_invitation fk-project_invitation-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_invitation
    ADD CONSTRAINT "fk-project_invitation-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: project_user fk-project_user-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_user
    ADD CONSTRAINT "fk-project_user-project" FOREIGN KEY (project) REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: project_user fk-project_user-role; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_user
    ADD CONSTRAINT "fk-project_user-role" FOREIGN KEY (role) REFERENCES public.role(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: project_user fk-project_user-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.project_user
    ADD CONSTRAINT "fk-project_user-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: role fk-role-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role
    ADD CONSTRAINT "fk-role-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: session fk-session-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.session
    ADD CONSTRAINT "fk-session-user" FOREIGN KEY (user_id) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: task fk-task-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task
    ADD CONSTRAINT "fk-task-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: task fk-task-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task
    ADD CONSTRAINT "fk-task-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: task_action fk-task_action-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_action
    ADD CONSTRAINT "fk-task_action-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON UPDATE CASCADE ON DELETE RESTRICT;


--
-- Name: task_action fk-task_action-task; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_action
    ADD CONSTRAINT "fk-task_action-task" FOREIGN KEY (task) REFERENCES public.task(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: task_action_delivery fk-task_action_delivery-action_id; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_action_delivery
    ADD CONSTRAINT "fk-task_action_delivery-action_id" FOREIGN KEY (action_id) REFERENCES public.task_action(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: task_trigger fk-task_trigger-task; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_trigger
    ADD CONSTRAINT "fk-task_trigger-task" FOREIGN KEY (task) REFERENCES public.task(id) ON UPDATE CASCADE ON DELETE CASCADE;


--
-- Name: upload_session fk-upload_session-project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upload_session
    ADD CONSTRAINT "fk-upload_session-project" FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: user_ssh_key fk-user_ssh_key-user; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_ssh_key
    ADD CONSTRAINT "fk-user_ssh_key-user" FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: worker_registration fk-worker_registration-created_by; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.worker_registration
    ADD CONSTRAINT "fk-worker_registration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id);


--
-- Name: api fk_api_project; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.api
    ADD CONSTRAINT fk_api_project FOREIGN KEY (project) REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE SET NULL;


--
-- Name: open_pr_state open_pr_state_action_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.open_pr_state
    ADD CONSTRAINT open_pr_state_action_fkey FOREIGN KEY (action) REFERENCES public.task_action(id) ON DELETE CASCADE;


--
-- Name: open_pr_state open_pr_state_task_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.open_pr_state
    ADD CONSTRAINT open_pr_state_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE;


--
-- Name: task_flake_input_override task_flake_input_override_task_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.task_flake_input_override
    ADD CONSTRAINT task_flake_input_override_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE;


--
-- Name: team_cache team_cache_cache_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache
    ADD CONSTRAINT team_cache_cache_fkey FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: team_cache_request team_cache_request_cache_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_cache_fkey FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: team_cache_request team_cache_request_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: team_cache_request team_cache_request_role_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_role_fkey FOREIGN KEY (role) REFERENCES public.cache_role(id) ON DELETE CASCADE;


--
-- Name: team_cache_request team_cache_request_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache_request
    ADD CONSTRAINT team_cache_request_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team_cache team_cache_role_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache
    ADD CONSTRAINT team_cache_role_fkey FOREIGN KEY (role) REFERENCES public.cache_role(id) ON DELETE CASCADE;


--
-- Name: team_cache team_cache_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_cache
    ADD CONSTRAINT team_cache_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team team_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team
    ADD CONSTRAINT team_created_by_fkey FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: team_invitation team_invitation_invited_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_invited_by_fkey FOREIGN KEY (invited_by) REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: team_invitation team_invitation_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team_invitation team_invitation_user_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_invitation
    ADD CONSTRAINT team_invitation_user_fkey FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: team team_new_project_role_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team
    ADD CONSTRAINT team_new_project_role_fkey FOREIGN KEY (new_project_role) REFERENCES public.role(id) ON DELETE SET NULL;


--
-- Name: team_project team_project_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project
    ADD CONSTRAINT team_project_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: team_project_request team_project_request_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: team_project_request team_project_request_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: team_project_request team_project_request_role_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_role_fkey FOREIGN KEY (role) REFERENCES public.role(id) ON DELETE CASCADE;


--
-- Name: team_project_request team_project_request_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project_request
    ADD CONSTRAINT team_project_request_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team_project team_project_role_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project
    ADD CONSTRAINT team_project_role_fkey FOREIGN KEY (role) REFERENCES public.role(id) ON DELETE CASCADE;


--
-- Name: team_project team_project_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_project
    ADD CONSTRAINT team_project_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team_user team_user_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: team_user team_user_user_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_user
    ADD CONSTRAINT team_user_user_fkey FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: team_worker team_worker_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_worker
    ADD CONSTRAINT team_worker_created_by_fkey FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: team_worker team_worker_team_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_worker
    ADD CONSTRAINT team_worker_team_fkey FOREIGN KEY (team) REFERENCES public.team(id) ON DELETE CASCADE;


--
-- Name: user_cache_star user_cache_star_cache_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_cache_star
    ADD CONSTRAINT user_cache_star_cache_fkey FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: user_cache_star user_cache_star_user_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_cache_star
    ADD CONSTRAINT user_cache_star_user_fkey FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_project_star user_project_star_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_project_star
    ADD CONSTRAINT user_project_star_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- Name: user_project_star user_project_star_user_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_project_star
    ADD CONSTRAINT user_project_star_user_fkey FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_task_star user_task_star_task_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_task_star
    ADD CONSTRAINT user_task_star_task_fkey FOREIGN KEY (task) REFERENCES public.task(id) ON DELETE CASCADE;


--
-- Name: user_task_star user_task_star_user_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_task_star
    ADD CONSTRAINT user_task_star_user_fkey FOREIGN KEY ("user") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: webhook webhook_cache_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT webhook_cache_fkey FOREIGN KEY (cache) REFERENCES public.cache(id) ON DELETE CASCADE;


--
-- Name: webhook webhook_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT webhook_created_by_fkey FOREIGN KEY (created_by) REFERENCES public."user"(id);


--
-- Name: webhook_delivery webhook_delivery_webhook_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery
    ADD CONSTRAINT webhook_delivery_webhook_id_fkey FOREIGN KEY (webhook_id) REFERENCES public.webhook(id) ON DELETE CASCADE;


--
-- Name: webhook webhook_project_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT webhook_project_fkey FOREIGN KEY (project) REFERENCES public.project(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
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
