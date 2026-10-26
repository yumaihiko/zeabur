// Components an action needs and the cluster lacks, as zeabur-installer names them.
import type { Schemas } from "@/api/client"

/** A component to install: its entry in the installer's `enable` and its name. */
export interface Needed {
  /** `capability/id`, or a capability for the installer to fill. */
  id: string
  name: string
}

/** Zeabur Builder is no catalogue component: the installer's `builds` brings it. */
export const BUILDER: Needed = { id: "builds", name: "Zeabur Builder" }

export const ZOT = "container-registry/zot"
export const KEEL = "image-updates/keel"
export const OBJECT_STORAGE = "object-storage/seaweedfs-operator"

/** `id` with the catalogue's name for it, else the last part of the id. */
export function needed(id: string, capabilities: Schemas["Capability"][] | undefined): Needed {
  if (id === BUILDER.id) return BUILDER
  const [capability = id, component] = id.split("/")
  const found = capabilities
    ?.find((c) => c.name === capability)
    ?.components.find((c) => c.id === component)
  return { id, name: found?.displayName ?? component ?? id }
}

/** What `zeaburctl components export` prints: catalogue components only. */
export const exportable = (list: Needed[]) => list.filter((n) => n.id.includes("/"))
