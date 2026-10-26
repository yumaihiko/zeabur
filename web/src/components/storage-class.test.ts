import { describe, expect, test } from "bun:test"

import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"

import { StorageClassField } from "@/components/storage-class"
import type { ProjectStorage, StorageClass } from "@/model/storage"

const local: StorageClass = {
  origin: { apiVersion: "storage.k8s.io/v1", kind: "StorageClass", name: "local-path" },
  name: "local-path",
  provisioner: "rancher.io/local-path",
  kind: "local",
  default: true,
  allowExpansion: false,
  reclaimPolicy: "Delete",
  bindingMode: "WaitForFirstConsumer",
}

const kept: StorageClass = {
  ...local,
  origin: { ...local.origin, name: "kept" },
  name: "kept",
  default: false,
  reclaimPolicy: "Retain",
}

const storage: ProjectStorage = {
  id: "projects/shop/storage",
  classes: [local, kept],
  claims: [],
  unreadable: [],
}

const field = (value: string) =>
  renderToStaticMarkup(createElement(StorageClassField, { storage, value, onChange: () => {} }))

describe("Choosing where a new volume goes", () => {
  test("says whether its data stays after the volume is deleted", () => {
    expect(field("local-path")).toContain("Deleting the volume deletes its data.")
    expect(field("kept")).toContain("Data stays after the volume is deleted.")
  })
})
