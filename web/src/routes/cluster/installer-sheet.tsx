import type { ReactNode } from "react"
import XIcon from "~icons/ph/x"

import { CopyButton } from "@/components/bits"
import { Button } from "@zeabur/design-system/components/button"
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogTitle,
} from "@zeabur/design-system/components/dialog"
import { useT } from "@/i18n"

const CONFIG = "/etc/zeabur-installer/config.yaml"

/** The installer commands that install or remove one component on the server. */
export function InstallerSheet({
  component,
  name,
  installed,
  onClose,
}: {
  /** `capability/id`. */
  component: string
  name: string
  /** Detection found this component in the cluster. */
  installed: boolean
  onClose: () => void
}) {
  const t = useT()
  return (
    <Dialog open onOpenChange={(open) => (open ? undefined : onClose())}>
      <DialogContent variant="panel">
        <div className="flex items-center gap-2 border-b px-6 py-3">
          <DialogTitle className="flex min-w-0 flex-1 items-baseline">
            <span className="truncate">{name}</span>
            <span className="ml-2 shrink-0 type-meta font-normal text-muted-foreground">
              {installed ? t("settings.removeCommand") : t("settings.installCommand")}
            </span>
          </DialogTitle>
          <DialogClose
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                className="-mr-1"
                aria-label={t("common.close")}
              />
            }
          >
            <XIcon aria-hidden="true" />
          </DialogClose>
        </div>
        <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-6 py-5">
          <p className="text-pretty">{t("installer.intro")}</p>
          <Step text={installed ? t("installer.guiRemove", name) : t("installer.gui", name)}>
            <Command text="sudo zeabur-installer gui" />
            <p className="type-meta text-pretty text-muted-foreground">{t("installer.guiReach")}</p>
          </Step>
          <Step
            text={installed ? t("installer.configRemove", CONFIG) : t("installer.config", CONFIG)}
          >
            {installed ? null : <Command text={`enable:\n  - ${component}`} />}
            <Command text="sudo zeabur-installer apply" />
          </Step>
          {installed ? (
            <div className="flex flex-col gap-1 type-meta text-pretty text-muted-foreground">
              <p>{t("installer.stays")}</p>
              <p>{t("installer.elsewhere")}</p>
            </div>
          ) : (
            <Step text={t("installer.otherCluster")}>
              <Command text={`zeaburctl components export ${component}`} />
            </Step>
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Step({ text, children }: { text: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2">
      <p className="text-pretty">{text}</p>
      {children}
    </section>
  )
}

function Command({ text }: { text: string }) {
  return (
    <div className="relative min-w-0 rounded-lg border bg-muted/30">
      <CopyButton value={text} className="absolute top-2 right-2" />
      <pre className="overflow-x-auto py-3 pr-12 pl-3 font-mono text-xs leading-5" translate="no">
        {text}
      </pre>
    </div>
  )
}
