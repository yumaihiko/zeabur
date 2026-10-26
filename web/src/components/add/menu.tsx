import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { cloneElement, useState } from "react"
import CaretDownIcon from "~icons/ph/caret-down"
import PlusIcon from "~icons/ph/plus"

import type { Schemas } from "@/api/client"
import {
  buildsQuery,
  capabilitiesQuery,
  clusterQuery,
  creatableQuery,
  databasesQuery,
  objectStorageQuery,
} from "@/api/queries"
import { GatedMenuItem, GateTip, useGates } from "@/components/gate"
import { LazyInstallerSheet } from "@/components/lazy-installer-sheet"
import { useT } from "@/i18n"
import { openAdd } from "@/lib/adding"
import { engineName, engineOperator, engines, type Engine } from "@/model/databases"
import { gitNeeds } from "@/model/git-service"
import { needed, OBJECT_STORAGE, type Needed } from "@/model/installer"
import type { Gate } from "@/model/rights"
import { Button } from "@zeabur/design-system/components/button"
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@zeabur/design-system/components/menu"

export interface AddChoices {
  image: boolean
  /** Offered engines; `installed` false offers the operator's install steps instead. */
  databases: { engine: Engine; installed: boolean; operator: Needed }[]
  /** Object storage, when the person may create it and whether its operator is installed is
   * known. */
  objectStorage?: { installed: boolean; operator: Needed }
  /** "Deploy from Git": open, or closed by the person's role or by what the cluster lacks. */
  git: { open: boolean; gate: Gate; install: Needed[]; registrySetting: boolean }
  yaml: boolean
  /** Nothing to offer: the person may only look. */
  none: boolean
  /** Why a service, or a database, cannot be added. */
  gates: { image: Gate; database: Gate }
}

type Capabilities = Schemas["Capability"][] | undefined

/** Engines whose operator is known to be installed or not. An operator whose databases can't be
 * read is left out. */
function knownEngines(
  operators: { engine: Engine; state: string }[] = [],
  capabilities: Capabilities,
) {
  return engines.flatMap((engine) => {
    const state = operators.find((o) => o.engine === engine)?.state
    return state === "installed" || state === "not-installed"
      ? [
          {
            engine,
            installed: state === "installed",
            operator: needed(engineOperator[engine], capabilities),
          },
        ]
      : []
  })
}

/** Object storage, when the role allows it and whether the operator is installed is known. */
function useObjectStorageChoice(project: string | undefined, capabilities: Capabilities) {
  const gate = useGates(project)("changeObjectStorage")
  const state = useQuery({ ...objectStorageQuery(project ?? ""), enabled: !!project }).data?.state
  if (!gate.open || (state !== "installed" && state !== "not-installed")) return undefined
  return { installed: state === "installed", operator: needed(OBJECT_STORAGE, capabilities) }
}

/** "Deploy from Git": open, or closed by the person's role or by what the cluster lacks. */
function useGitChoice(project: string | undefined, capabilities: Capabilities): AddChoices["git"] {
  const builds = useQuery({
    ...buildsQuery(project ?? ""),
    enabled: !!project,
    refetchInterval: false,
  })
  const cluster = useQuery(clusterQuery).data
  const gate = useGates(project)("deployFromGit")
  const needs = gitNeeds({
    builds: builds.data?.state,
    capabilities,
    registry: cluster && (cluster.builds.registry ?? ""),
  })
  return gitChoice(gate, needs, capabilities)
}

function gitChoice(
  gate: Gate,
  needs: ReturnType<typeof gitNeeds>,
  capabilities: Capabilities,
): AddChoices["git"] {
  if (!needs) return { open: gate.open, gate, install: [], registrySetting: false }
  return {
    open: false,
    gate,
    install: needs.install.map((id) => needed(id, capabilities)),
    registrySetting: needs.registrySetting,
  }
}

/** What "New" offers in the project: a kind the person's role cannot add is closed, one unknown
 * is offered and the plan's dry run decides. */
export function useAddChoices(project: string | undefined): AddChoices | undefined {
  const creatable = useQuery({ ...creatableQuery(project ?? ""), enabled: !!project })
  const databases = useQuery({ ...databasesQuery(project ?? ""), enabled: !!project })
  // Readable by cluster administrators only; for anyone else what is installed stays unknown.
  const capabilities = useQuery({ ...capabilitiesQuery, retry: false }).data?.items
  const gates = useGates(project)
  const store = useObjectStorageChoice(project, capabilities)
  const git = useGitChoice(project, capabilities)
  if (!project || creatable.isPending) return undefined
  const imageGate = gates("deploy")
  const databaseGate = gates("changeDatabases")
  const offered = databaseGate.open ? knownEngines(databases.data?.operators, capabilities) : []
  const yaml = (creatable.data?.items ?? []).some((k) => k.access !== "denied")
  return {
    image: imageGate.open,
    git,
    databases: offered,
    objectStorage: store,
    yaml,
    none: !imageGate.open && offered.length === 0 && !store && !yaml,
    gates: { image: imageGate, database: databaseGate },
  }
}

/** The install steps a closed item opens. */
interface Steps {
  components: Needed[]
  reason?: string
  note?: string
}

/** "New": the one entry to add a service, a database or YAML to the project. */
export function AddMenu({ project, compact = false }: { project: string; compact?: boolean }) {
  const t = useT()
  const choices = useAddChoices(project)
  const [steps, setSteps] = useState<Steps>()
  if (!choices) return null
  const trigger = compact ? (
    <Button variant="ghost" size="icon-xs" aria-label={t("add.inProject", project)} />
  ) : (
    <Button size="sm" data-testid="add" />
  )
  const face = (
    <>
      <PlusIcon aria-hidden="true" data-icon={compact ? undefined : "inline-start"} />
      {compact ? null : (
        <>
          {t("add.button")}
          <CaretDownIcon aria-hidden="true" data-icon="inline-end" />
        </>
      )}
    </>
  )
  // A Viewer sees what Developers use to add things, closed, with the role that opens it.
  if (choices.none)
    return <GateTip gate={choices.gates.image}>{cloneElement(trigger, {}, face)}</GateTip>
  const databases = choices.databases
  const store = choices.objectStorage
  return (
    <>
      <Menu>
        <MenuTrigger render={trigger}>{face}</MenuTrigger>
        <MenuContent align={compact ? "start" : "end"} className="w-60">
          <GatedMenuItem
            gate={choices.gates.image}
            testid="add-image"
            onClick={() => openAdd(project, "image")}
          >
            {t("add.image")}
          </GatedMenuItem>
          <GitItem project={project} git={choices.git} what="git" onSteps={setSteps} />
          <GitItem project={project} git={choices.git} what="upload" onSteps={setSteps} />
          <GatedMenuItem
            gate={choices.gates.image}
            testid="add-compose"
            onClick={() => openAdd(project, "compose")}
          >
            {t("add.compose")}
          </GatedMenuItem>
          <GatedMenuItem
            gate={choices.gates.image}
            testid="add-template"
            onClick={() => openAdd(project, "template")}
          >
            {t("add.template")}
          </GatedMenuItem>
          {!choices.gates.database.open ? (
            <>
              <MenuSeparator />
              <GatedMenuItem gate={choices.gates.database}>{t("add.database")}</GatedMenuItem>
            </>
          ) : null}
          {databases.length > 0 ? <MenuSeparator /> : null}
          {databases.map(({ engine, installed, operator }) => (
            <OperatorItem
              key={engine}
              label={engineName(engine)}
              testid={`add-${engine}`}
              installed={installed}
              operator={operator}
              onOpen={() => openAdd(project, engine)}
              onSteps={setSteps}
            />
          ))}
          {store ? (
            <>
              <MenuSeparator />
              <OperatorItem
                label={t("s3.title")}
                testid="add-object-storage"
                installed={store.installed}
                operator={store.operator}
                onOpen={() => openAdd(project, "objectStorage")}
                onSteps={setSteps}
              />
            </>
          ) : null}
          {choices.yaml ? (
            <>
              <MenuSeparator />
              <MenuLabel>{t("add.advanced")}</MenuLabel>
              <MenuItem render={<Link to="/projects/$project/create" params={{ project }} />}>
                {t("add.yaml")}
              </MenuItem>
            </>
          ) : null}
        </MenuContent>
      </Menu>
      {steps ? <LazyInstallerSheet {...steps} onClose={() => setSteps(undefined)} /> : null}
    </>
  )
}

/** A database engine or object storage, or what to install before it. */
function OperatorItem({
  label,
  testid,
  installed,
  operator,
  onOpen,
  onSteps,
}: {
  label: string
  testid: string
  installed: boolean
  operator: Needed
  onOpen: () => void
  onSteps: (steps: Steps) => void
}) {
  const t = useT()
  if (installed)
    return (
      <MenuItem data-testid={testid} onClick={onOpen}>
        {label}
      </MenuItem>
    )
  return (
    <NeedsItem
      label={label}
      why={t("add.installFirst", operator.name)}
      onClick={() => onSteps({ components: [operator] })}
    />
  )
}

/** An item the cluster cannot serve yet: its label, what it lacks, and the install steps. */
function NeedsItem({
  label,
  why,
  testid,
  onClick,
}: {
  label: string
  why: string
  testid?: string
  onClick?: () => void
}) {
  return (
    <MenuItem data-testid={testid} disabled={!onClick} onClick={onClick}>
      <span className="flex min-w-0 flex-col gap-0.5">
        <span className="text-muted-foreground">{label}</span>
        <span className="type-meta text-muted-foreground">{why}</span>
      </span>
    </MenuItem>
  )
}

/** "Deploy from Git" or "Upload a folder", which need the same of the cluster; what it lacks
 * closes them, with the steps that install all of it. */
function GitItem({
  project,
  git,
  what,
  onSteps,
}: {
  project: string
  git: AddChoices["git"]
  what: "git" | "upload"
  onSteps: (steps: Steps) => void
}) {
  const t = useT()
  if (git.open || !git.gate.open)
    return (
      <GatedMenuItem gate={git.gate} testid={`add-${what}`} onClick={() => openAdd(project, what)}>
        {t(`add.${what}`)}
      </GatedMenuItem>
    )
  const note = git.registrySetting ? t("add.gitMissing.registrySetting") : undefined
  if (git.install.length === 0)
    return <NeedsItem label={t(`add.${what}`)} why={note ?? ""} testid={`add-${what}`} />
  const names = t(
    "common.list",
    git.install.map((n) => n.name),
  )
  return (
    <NeedsItem
      label={t(`add.${what}`)}
      why={t("add.installFirst", names)}
      testid={`add-${what}`}
      onClick={() => onSteps({ components: git.install, reason: t("add.gitNeedsWhy"), note })}
    />
  )
}
