import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { useSearchParams } from "react-router";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import * as QRCode from "qrcode";
import type { LucideIcon } from "lucide-react";
import { Check, Copy, Cpu, Eye, EyeOff, ExternalLink, FolderOpen, Globe2, KeyRound, MonitorCog, QrCode, RadioTower, RefreshCw, Server, Smartphone, Square, Trash2, Wifi } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel, FieldLegend, FieldSet, FieldTitle } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from "@/components/ui/input-group";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "@/components/ui/item";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import { api, isTauriRuntime, messageOf } from "../../shared/api";
import { createTauriListenerCleanup } from "../../shared/tauriEvent";
import type { LaboratoryRole, LaboratoryServerPreferences, LaboratoryServerRecord, LaboratoryStatus } from "../../shared/types";
import { useAppConfig } from "../../features/config/AppConfigProvider";
import { useSettingsContext } from "../settings";
import styles from "../settings.module.scss";
import { PageHeader, SettingsPage, SettingsSection } from "./components";

type LaboratoryDirection = "sync" | "web" | "hardware";
type LaboratoryRun = (operation: () => Promise<LaboratoryStatus | unknown>, notice?: string) => Promise<void>;

function directionFromQuery(value: string | null): LaboratoryDirection {
  return value === "web" || value === "hardware" ? value : "sync";
}

function phaseLabel(status: LaboratoryStatus, t: TFunction) {
  return t(`settings.laboratory.phase.${status.phase}`);
}

function formatDate(value: number | null, fallback: string) {
  return value ? new Date(value).toLocaleString() : fallback;
}

function LaboratorySection({ id, title, description, footer, children }: { id: string; title?: string; description?: ReactNode; footer?: ReactNode; children: ReactNode }) {
  return (
    <SettingsSection id={id} title={title}>
      {description && <p className={styles.cardHint}>{description}</p>}
      {children}
      {footer && <div className={styles.laboratorySectionFooter}>{footer}</div>}
    </SettingsSection>
  );
}

function LaboratoryTextRow({ label, description, value, emptyValue, disabled = false, onChange }: { label: string; description?: string; value: string; emptyValue: string; disabled?: boolean; onChange: (value: string) => void }) {
  const [draft, setDraft] = useState(value);

  useEffect(() => setDraft(value), [value]);

  const commit = () => {
    const next = draft.trim() || emptyValue;
    setDraft(next);
    if (next !== value) onChange(next);
  };

  return (
    <Field orientation="responsive" className={cn(styles.settingRow, styles.laboratoryTextRow)} data-disabled={disabled || undefined}>
      <FieldContent>
        <FieldTitle>{label}</FieldTitle>
        {description && <FieldDescription>{description}</FieldDescription>}
      </FieldContent>
      <Input className="w-full max-w-sm" aria-label={label} disabled={disabled} spellCheck={false} value={draft} onBlur={commit} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }} />
    </Field>
  );
}

function DirectionSelector({ value, onChange, t }: { value: LaboratoryDirection; onChange: (value: LaboratoryDirection) => void; t: TFunction }) {
  const directions: Array<{ id: LaboratoryDirection; label: string; icon: LucideIcon }> = [
    { id: "sync", label: t("settings.laboratory.directions.sync"), icon: RadioTower },
    { id: "web", label: t("settings.laboratory.directions.web"), icon: Globe2 },
    { id: "hardware", label: t("settings.laboratory.directions.hardware"), icon: Cpu },
  ];

  return (
    <ToggleGroup
      className={cn(styles.lyricsModeSelector, styles.laboratoryDirectionSelector)}
      variant="outline"
      value={[value]}
      onValueChange={(values) => {
        const next = values[0] as LaboratoryDirection | undefined;
        if (next) onChange(next);
      }}
      aria-label={t("settings.laboratory.directions.title")}
    >
      {directions.map((direction) => {
        const Icon = direction.icon;
        return (
          <ToggleGroupItem key={direction.id} value={direction.id} aria-label={direction.label}>
            <Icon aria-hidden="true" />
            <span>{direction.label}</span>
          </ToggleGroupItem>
        );
      })}
    </ToggleGroup>
  );
}

function RuntimeSection({ status, autoStart, busy, run, t }: { status: LaboratoryStatus; autoStart: boolean; busy: boolean; run: LaboratoryRun; t: TFunction }) {
  const isRunning = status.running;
  const isServer = status.role === "server";

  return (
    <LaboratorySection
      id="laboratory-role"
      title={t("settings.laboratory.sections.role")}
    >
      <FieldGroup className={styles.laboratoryFieldGroup}>
        <Field orientation="vertical" className={styles.settingRow} data-disabled={(busy || isRunning) || undefined}>
          <FieldContent>
            <FieldTitle>{t("settings.laboratory.roleTitle")}</FieldTitle>
            <FieldDescription>{t("settings.laboratory.roleHint")}</FieldDescription>
          </FieldContent>
          <ToggleGroup
            className={styles.laboratoryRoleSelector}
            variant="outline"
            value={[status.role]}
            disabled={busy || isRunning}
            onValueChange={(values) => {
              const role = values[0] as LaboratoryRole | undefined;
              if (role && role !== status.role) void run(() => api.setRole(role));
            }}
            aria-label={t("settings.laboratory.roleTitle")}
          >
            <ToggleGroupItem value="server" aria-label={t("settings.laboratory.serverRole")}><Server aria-hidden="true" />{t("settings.laboratory.serverRole")}</ToggleGroupItem>
            <ToggleGroupItem value="client" aria-label={t("settings.laboratory.clientRole")}><Smartphone aria-hidden="true" />{t("settings.laboratory.clientRole")}</ToggleGroupItem>
          </ToggleGroup>
        </Field>
        <div className={styles.laboratoryRuntimeActions}>
          {isRunning ? (
            <Button variant="destructive" onClick={() => void run(() => api.stop(), t("settings.laboratory.stopped"))} disabled={busy}>
              <Square data-icon="inline-start" />{t("settings.laboratory.stop")}
            </Button>
          ) : (
            <Button onClick={() => void run(() => api.start(), t("settings.laboratory.started"))} disabled={busy}>
              <Wifi data-icon="inline-start" />{t("settings.laboratory.start")}
            </Button>
          )}
          <Badge variant={status.phase === "error" ? "destructive" : status.running ? "default" : "secondary"}>{phaseLabel(status, t)}</Badge>
        </div>
        <Field orientation="responsive" className={styles.settingRow} data-disabled={busy || undefined}>
          <FieldContent>
            <FieldTitle>{t("settings.laboratory.autoStart")}</FieldTitle>
            <FieldDescription>{t("settings.laboratory.autoStartHint")}</FieldDescription>
          </FieldContent>
          <Switch checked={autoStart} disabled={busy} onCheckedChange={(enabled) => void run(() => api.setAutoStart(enabled))} aria-label={t("settings.laboratory.autoStart")} />
        </Field>
      </FieldGroup>
      {status.message && (
        <Alert className={styles.laboratoryInlineAlert} variant={status.phase === "error" ? "destructive" : "default"}>
          <AlertTitle>{isServer ? t("settings.laboratory.serverRole") : t("settings.laboratory.clientRole")}</AlertTitle>
          <AlertDescription>{status.message}</AlertDescription>
        </Alert>
      )}
    </LaboratorySection>
  );
}

function ServerSharedFields({ draft, onPortChange, onDebounceChange, onSave, t }: { draft: LaboratoryServerPreferences; onPortChange: (value: number) => void; onDebounceChange: (value: number) => void; onSave: () => void; t: TFunction }) {
  return (
    <>
      <Field orientation="responsive" className={styles.settingRow}>
        <FieldContent>
          <FieldTitle>{t("settings.laboratory.port")}</FieldTitle>
          <FieldDescription>{t("settings.laboratory.portHint")}</FieldDescription>
        </FieldContent>
        <Input className="w-full max-w-xs" type="number" min={1024} max={65535} value={draft.port} onChange={(event) => onPortChange(Number(event.target.value))} onBlur={onSave} />
      </Field>
      <Field orientation="responsive" className={styles.settingRow}>
        <FieldContent>
          <FieldTitle>{t("settings.laboratory.debounce")}</FieldTitle>
          <FieldDescription>{t("settings.laboratory.debounceHint")}</FieldDescription>
        </FieldContent>
        <Input className="w-full max-w-xs" type="number" min={50} max={10000} value={draft.debounceMs} onChange={(event) => onDebounceChange(Number(event.target.value))} onBlur={onSave} />
      </Field>
    </>
  );
}

type ServerSettingsSectionProps = {
  title: string;
  description?: ReactNode;
  draft: LaboratoryServerPreferences;
  status: LaboratoryStatus;
  password: string;
  busy: boolean;
  showDiscovery: boolean;
  showPassword: boolean;
  onNameChange: (value: string) => void;
  onPortChange: (value: number) => void;
  onDebounceChange: (value: number) => void;
  onSave: () => void;
  onDiscoveryChange: (value: boolean) => void;
  onPasswordChange: (value: string) => void;
  onSavePassword: () => void;
  t: TFunction;
};

function ServerSettingsSection({ title, description, draft, status, password, busy, showDiscovery, showPassword, onNameChange, onPortChange, onDebounceChange, onSave, onDiscoveryChange, onPasswordChange, onSavePassword, t }: ServerSettingsSectionProps) {
  const [passwordVisible, setPasswordVisible] = useState(false);

  return (
    <LaboratorySection id="laboratory-server" title={title} description={description}>
      <FieldGroup className={styles.laboratoryFieldGroup}>
        <LaboratoryTextRow label={t("settings.laboratory.serverName")} description={t("settings.laboratory.serverNameHint")} value={draft.name} emptyValue={status.serverId} onChange={onNameChange} />
        <ServerSharedFields draft={draft} onPortChange={onPortChange} onDebounceChange={onDebounceChange} onSave={onSave} t={t} />
        {showDiscovery && (
          <>
            <Field orientation="responsive" className={styles.settingRow}>
              <FieldContent>
                <FieldTitle>{t("settings.laboratory.discovery")}</FieldTitle>
                <FieldDescription>{t("settings.laboratory.discoveryHint")}</FieldDescription>
              </FieldContent>
              <Switch checked={draft.discoveryEnabled} disabled={busy} onCheckedChange={onDiscoveryChange} aria-label={t("settings.laboratory.discovery")} />
            </Field>
            <Field orientation="responsive" className={styles.settingRow}>
              <FieldContent>
                <FieldTitle>{t("settings.laboratory.serverId")}</FieldTitle>
                <FieldDescription>{t("settings.laboratory.serverIdHint")}</FieldDescription>
              </FieldContent>
              <code className={styles.laboratoryIdValue}>{status.serverId}</code>
            </Field>
          </>
        )}
        {showPassword && (
          <Field className={cn(styles.settingRow, styles.laboratoryPasswordRow)} data-disabled={busy || undefined}>
            <FieldContent>
              <FieldTitle>{t("settings.laboratory.savePassword")}</FieldTitle>
              <FieldDescription>{status.serverPasswordEnabled ? t("settings.laboratory.passwordOn") : t("settings.laboratory.passwordOff")}</FieldDescription>
            </FieldContent>
            <InputGroup className={styles.laboratoryPasswordGroup}>
              <InputGroupAddon><KeyRound aria-hidden="true" /></InputGroupAddon>
              <InputGroupInput type={passwordVisible ? "text" : "password"} value={password} placeholder={status.serverPasswordEnabled ? t("settings.laboratory.passwordConfigured") : t("settings.laboratory.passwordPlaceholder")} onChange={(event) => onPasswordChange(event.target.value)} aria-label={t("settings.laboratory.savePassword")} />
              <InputGroupAddon align="inline-end">
                <InputGroupButton
                  size="icon-sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label={t(passwordVisible ? "settings.laboratory.hidePassword" : "settings.laboratory.showPassword")}
                  aria-pressed={passwordVisible}
                  onClick={() => setPasswordVisible((visible) => !visible)}
                >
                  {passwordVisible ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
                </InputGroupButton>
                <InputGroupButton size="sm" variant="outline" disabled={busy} onClick={onSavePassword}>{t("settings.laboratory.savePassword")}</InputGroupButton>
              </InputGroupAddon>
            </InputGroup>
          </Field>
        )}
      </FieldGroup>
    </LaboratorySection>
  );
}

function ClientsSection({ status, busy, run, t }: { status: LaboratoryStatus; busy: boolean; run: LaboratoryRun; t: TFunction }) {
  return (
    <SettingsSection id="laboratory-clients" title={t("settings.laboratory.clientsTitle")}>
      {status.clients.length === 0 ? (
        <Empty className={styles.laboratoryEmpty}>
          <EmptyHeader>
            <EmptyMedia variant="icon"><MonitorCog /></EmptyMedia>
            <EmptyTitle>{t("settings.laboratory.clientsEmpty")}</EmptyTitle>
          </EmptyHeader>
        </Empty>
      ) : (
        <ItemGroup className={styles.laboratoryItemGroup}>
          {status.clients.map((client) => (
            <Item key={client.clientId} variant="muted" size="sm">
              <ItemMedia variant="icon"><MonitorCog /></ItemMedia>
              <ItemContent className="min-w-0">
                <ItemTitle>{client.name}</ItemTitle>
                <ItemDescription>{client.clientId} · {client.online ? t("settings.laboratory.online") : t("settings.laboratory.offline")} · {formatDate(client.lastConnectedAtMs, t("settings.laboratory.never"))}</ItemDescription>
              </ItemContent>
              <ItemActions>
                {client.online ? (
                  <Button size="sm" variant="outline" disabled={busy} onClick={() => void run(() => api.kickClient(client.clientId), t("settings.laboratory.kicked"))}>{t("settings.laboratory.kick")}</Button>
                ) : (
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => api.forgetClient(client.clientId), t("settings.laboratory.forgotten"))}><Trash2 data-icon="inline-start" />{t("settings.laboratory.forget")}</Button>
                )}
              </ItemActions>
            </Item>
          ))}
        </ItemGroup>
      )}
    </SettingsSection>
  );
}

function RecentServersSection({ status, busy, connectionPasswords, setConnectionPassword, connect, t }: { status: LaboratoryStatus; busy: boolean; connectionPasswords: Record<string, string>; setConnectionPassword: (serverId: string, password: string) => void; connect: (record: LaboratoryServerRecord) => void; t: TFunction }) {
  return (
    <SettingsSection id="laboratory-server-list" title={t("settings.laboratory.serverListTitle")}>
      {status.recentServers.length === 0 ? (
        <Empty className={styles.laboratoryEmpty}>
          <EmptyHeader>
            <EmptyMedia variant="icon"><Server /></EmptyMedia>
            <EmptyTitle>{t("settings.laboratory.serversEmpty")}</EmptyTitle>
          </EmptyHeader>
        </Empty>
      ) : (
        <ItemGroup className={styles.laboratoryItemGroup}>
          {status.recentServers.map((record) => (
            <Item key={record.serverId} variant="muted" size="sm">
              <ItemMedia variant="icon"><Server /></ItemMedia>
              <ItemContent className="min-w-0">
                <ItemTitle>{record.name}</ItemTitle>
                <ItemDescription>{record.address}:{record.port} · {record.requiresPassword ? t("settings.laboratory.passwordRequired") : t("settings.laboratory.openAccess")} · {formatDate(record.lastConnectedAtMs, t("settings.laboratory.never"))}</ItemDescription>
                {record.requiresPassword && (
                  <InputGroup className={styles.laboratoryListPassword}>
                    <InputGroupAddon><KeyRound aria-hidden="true" /></InputGroupAddon>
                    <InputGroupInput type="password" placeholder={t("settings.laboratory.passwordPlaceholder")} value={connectionPasswords[record.serverId] ?? ""} onChange={(event) => setConnectionPassword(record.serverId, event.target.value)} aria-label={`${record.name} ${t("settings.laboratory.passwordPlaceholder")}`} />
                  </InputGroup>
                )}
              </ItemContent>
              <ItemActions><Button size="sm" variant="outline" disabled={busy} onClick={() => connect(record)}>{t("settings.laboratory.connect")}</Button></ItemActions>
            </Item>
          ))}
        </ItemGroup>
      )}
    </SettingsSection>
  );
}

function ClientSettingsSection({ status, clientName, busy, manual, setManual, run, submitManual, t }: { status: LaboratoryStatus; clientName: string; busy: boolean; manual: { address: string; port: string; name: string; password: string }; setManual: (value: { address: string; port: string; name: string; password: string }) => void; run: LaboratoryRun; submitManual: () => void; t: TFunction }) {
  return (
    <SettingsSection id="laboratory-client" title={t("settings.laboratory.clientSettingsTitle")}>
      <FieldGroup className={styles.laboratoryFieldGroup}>
        <LaboratoryTextRow label={t("settings.laboratory.clientName")} description={t("settings.laboratory.clientNameHint")} value={clientName} emptyValue={status.clientId} onChange={(name) => void run(() => api.setClientSettings({ name }))} />
        <Field orientation="responsive" className={styles.settingRow}>
          <FieldContent>
            <FieldTitle>{t("settings.laboratory.clientId")}</FieldTitle>
            <FieldDescription>{t("settings.laboratory.clientIdHint")}</FieldDescription>
          </FieldContent>
          <code className={styles.laboratoryIdValue}>{status.clientId}</code>
        </Field>
      </FieldGroup>
      <div className={styles.buttonRow}>
        <Button variant="outline" disabled={busy} onClick={() => void run(() => api.scanServers(), t("settings.laboratory.scanDone"))}><RefreshCw data-icon="inline-start" />{t("settings.laboratory.scan")}</Button>
        {status.phase === "error" && <Button variant="outline" disabled={busy} onClick={() => void run(() => api.retryConnection(), t("settings.laboratory.connecting"))}><RefreshCw data-icon="inline-start" />{t("settings.laboratory.retry")}</Button>}
      </div>
      <Separator />
      <FieldSet className={styles.laboratoryManualFieldSet}>
        <FieldLegend variant="label">{t("settings.laboratory.manualTitle")}</FieldLegend>
        <FieldGroup className={styles.laboratoryManualFieldGroup}>
          <Field orientation="responsive">
            <FieldLabel htmlFor="laboratory-address">{t("settings.laboratory.addressPlaceholder")}</FieldLabel>
            <Input className="w-full max-w-sm" id="laboratory-address" value={manual.address} placeholder={t("settings.laboratory.addressPlaceholder")} onChange={(event) => setManual({ ...manual, address: event.target.value })} />
          </Field>
          <Field orientation="responsive">
            <FieldLabel htmlFor="laboratory-port">{t("settings.laboratory.port")}</FieldLabel>
            <Input className="w-full max-w-sm" id="laboratory-port" type="number" min={1024} max={65535} value={manual.port} onChange={(event) => setManual({ ...manual, port: event.target.value })} />
          </Field>
          <Field orientation="responsive">
            <FieldLabel htmlFor="laboratory-password">{t("settings.laboratory.passwordPlaceholder")}</FieldLabel>
            <InputGroup className="w-full max-w-sm">
              <InputGroupAddon><KeyRound aria-hidden="true" /></InputGroupAddon>
              <InputGroupInput id="laboratory-password" type="password" value={manual.password} placeholder={t("settings.laboratory.passwordPlaceholder")} onChange={(event) => setManual({ ...manual, password: event.target.value })} />
            </InputGroup>
          </Field>
        </FieldGroup>
        <Button className="self-start" disabled={busy} onClick={submitManual}>{t("settings.laboratory.connect")}</Button>
      </FieldSet>
    </SettingsSection>
  );
}

function WebServiceSection({ status, draft, busy, isServer, qrDataUrl, selectedAddress, selectedWebAddress, onAddressSelect, onWebEnabledChange, copyWebUrl, resetWebToken, t }: { status: LaboratoryStatus; draft: LaboratoryServerPreferences; busy: boolean; isServer: boolean; qrDataUrl: string | null; selectedAddress: string | null; selectedWebAddress: LaboratoryStatus["webAddresses"][number] | null; onAddressSelect: (address: string) => void; onWebEnabledChange: (value: boolean) => void; copyWebUrl: () => void; resetWebToken: () => void; t: TFunction }) {
  const hasAddresses = status.webAddresses.length > 0;

  return (
    <LaboratorySection
      id="laboratory-web"
      title={t("settings.laboratory.webTitle")}
      footer={hasAddresses ? <Button variant="outline" size="sm" disabled={busy} onClick={resetWebToken}>{t("settings.laboratory.resetToken")}</Button> : undefined}
    >
      <FieldGroup className={styles.laboratoryFieldGroup}>
        <Field orientation="responsive" className={styles.settingRow} data-disabled={!isServer || busy || undefined}>
          <FieldContent>
            <FieldTitle>{t("settings.laboratory.webEnabled")}</FieldTitle>
            <FieldDescription>{t("settings.laboratory.webEnabledHint")}</FieldDescription>
          </FieldContent>
          <Switch checked={draft.webEnabled} disabled={!isServer || busy} onCheckedChange={onWebEnabledChange} aria-label={t("settings.laboratory.webEnabled")} />
        </Field>
      </FieldGroup>
      {!isServer ? (
        <Alert className={styles.laboratoryInlineAlert} variant="warning">
          <AlertTitle>{t("settings.laboratory.serverRole")}</AlertTitle>
          <AlertDescription>{t("settings.shell.webRequiresServer")}</AlertDescription>
        </Alert>
      ) : hasAddresses ? (
        <>
          <FieldGroup className={styles.laboratoryFieldGroup}>
            <Field className={cn(styles.settingRow, styles.laboratoryUrlRow)}>
              <FieldContent>
                <FieldTitle>{t("settings.laboratory.webAddress")}</FieldTitle>
                <FieldDescription>{t("settings.laboratory.webAddressHint")}</FieldDescription>
              </FieldContent>
              <ItemGroup className={styles.laboratoryWebAddressGroup}>
                {status.webAddresses.map((webAddress) => {
                  const selected = webAddress.address === selectedAddress;
                  return (
                    <Item
                      key={webAddress.address}
                      variant={selected ? "outline" : "muted"}
                      size="sm"
                      render={<button type="button" aria-pressed={selected} onClick={() => onAddressSelect(webAddress.address)} />}
                    >
                      <ItemMedia variant="icon">{selected ? <Check aria-hidden="true" /> : <Globe2 aria-hidden="true" />}</ItemMedia>
                      <ItemContent className="min-w-0">
                        <ItemTitle>
                          {webAddress.address}
                          {selected && <Badge variant="secondary">{t("settings.laboratory.selected")}</Badge>}
                        </ItemTitle>
                        <ItemDescription className={styles.laboratoryWebAddressUrl}>{webAddress.url}</ItemDescription>
                      </ItemContent>
                    </Item>
                  );
                })}
              </ItemGroup>
            </Field>
          </FieldGroup>
          {selectedWebAddress && (
            <>
              <FieldGroup className={styles.laboratoryFieldGroup}>
                <Field className={cn(styles.settingRow, styles.laboratoryUrlRow)}>
                  <FieldContent><FieldTitle>{t("settings.laboratory.selectedAddress")}</FieldTitle></FieldContent>
                  <InputGroup className={styles.laboratoryUrlGroup}>
                    <InputGroupInput readOnly value={selectedWebAddress.url} aria-label={t("settings.laboratory.selectedAddress")} />
                    <InputGroupAddon align="inline-end">
                      <InputGroupButton size="icon-sm" variant="ghost" aria-label={t("settings.laboratory.copyLink")} onClick={copyWebUrl}><Copy data-icon="inline-start" /></InputGroupButton>
                      <InputGroupButton size="icon-sm" variant="ghost" aria-label={t("settings.laboratory.openLink")} onClick={() => window.open(selectedWebAddress.url, "_blank")}><ExternalLink data-icon="inline-start" /></InputGroupButton>
                    </InputGroupAddon>
                  </InputGroup>
                </Field>
              </FieldGroup>
              <div className={styles.laboratoryQrCodeCard}>
                {qrDataUrl ? <img src={qrDataUrl} alt={t("settings.laboratory.qrAlt")} /> : <QrCode aria-hidden="true" />}
                <span>{t("settings.laboratory.qrHint")}</span>
              </div>
            </>
          )}
        </>
      ) : (
        <Empty className={styles.laboratoryEmpty}>
          <EmptyHeader>
            <EmptyMedia variant="icon"><Globe2 /></EmptyMedia>
            <EmptyTitle>{draft.webEnabled && status.running ? t("settings.laboratory.webAddressesEmpty") : draft.webEnabled ? t("settings.shell.webRequiresStart") : t("settings.shell.webDisabledHint")}</EmptyTitle>
            {draft.webEnabled && status.running && <EmptyDescription>{t("settings.laboratory.webAddressesEmptyHint")}</EmptyDescription>}
          </EmptyHeader>
        </Empty>
      )}
    </LaboratorySection>
  );
}

function ThemesSection({ status, busy, onOpenDirectory, t }: { status: LaboratoryStatus; busy: boolean; onOpenDirectory: () => void; t: TFunction }) {
  return (
    <LaboratorySection
      id="laboratory-themes"
      title={t("settings.laboratory.themesTitle")}
      description={t("settings.laboratory.themesHint")}
      footer={<Button variant="outline" size="sm" disabled={busy} onClick={onOpenDirectory} aria-label={`${t("common.actions.open")}: ${t("settings.laboratory.themesPath")}`}><FolderOpen data-icon="inline-start" />{t("common.actions.open")}</Button>}
    >
      {status.themes.length === 0 ? (
        <Empty className={styles.laboratoryEmpty}>
          <EmptyHeader>
            <EmptyMedia variant="icon"><MonitorCog /></EmptyMedia>
            <EmptyTitle>{t("settings.laboratory.themesEmpty")}</EmptyTitle>
          </EmptyHeader>
        </Empty>
      ) : (
        <ItemGroup className={styles.laboratoryItemGroup}>
          {status.themes.map((theme) => (
            <Item key={theme.id} variant="muted" size="sm">
              <ItemMedia variant="icon"><MonitorCog /></ItemMedia>
              <ItemContent className="min-w-0"><ItemTitle>{theme.name}</ItemTitle><ItemDescription>{theme.id} · v{theme.version} · SDK {theme.sdkVersion}</ItemDescription></ItemContent>
            </Item>
          ))}
        </ItemGroup>
      )}
      <p className={styles.directoryPath}>{t("settings.laboratory.themesPath")}</p>
    </LaboratorySection>
  );
}

function HardwareDirection({ t }: { t: TFunction }) {
  return (
    <LaboratorySection id="laboratory-hardware" title={t("settings.laboratory.directions.hardware")} description={t("settings.laboratory.directions.hardwareHint")}>
      <Empty className={styles.laboratoryEmpty}>
        <EmptyHeader>
          <EmptyMedia variant="icon"><Cpu /></EmptyMedia>
          <EmptyTitle>{t("settings.laboratory.directions.developing")}</EmptyTitle>
          <EmptyDescription>{t("settings.laboratory.directions.hardwareHint")}</EmptyDescription>
        </EmptyHeader>
      </Empty>
    </LaboratorySection>
  );
}

export default function LaboratorySettingsPage() {
  const { config } = useAppConfig();
  const { setError, setNotice } = useSettingsContext();
  const { t } = useTranslation();
  const [searchParams, setSearchParams] = useSearchParams();
  const direction = directionFromQuery(searchParams.get("direction"));
  const [status, setStatus] = useState<LaboratoryStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [password, setPassword] = useState("");
  const [connectionPasswords, setConnectionPasswords] = useState<Record<string, string>>({});
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [selectedWebAddressKey, setSelectedWebAddressKey] = useState<string | null>(null);
  const scannedRef = useRef(false);
  const [manual, setManual] = useState({ address: "", port: String(config.laboratory.server.port), name: "", password: "" });
  const [serverDraft, setServerDraft] = useState(() => config.laboratory.server);

  const refresh = async () => {
    try {
      setStatus(await api.getStatus());
    } catch (error) {
      setError(messageOf(error));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
    if (!isTauriRuntime()) return;
    return createTauriListenerCleanup(
      listen<LaboratoryStatus>("laboratory://status", ({ payload }) => setStatus(payload)),
    );
  }, []);

  useEffect(() => {
    if (direction !== "web" || status?.role !== "server" || !status.running) return;
    const interval = window.setInterval(() => void refresh(), 5_000);
    return () => window.clearInterval(interval);
  }, [direction, status?.role, status?.running]);

  useEffect(() => {
    setServerDraft(config.laboratory.server);
    setManual((current) => ({ ...current, port: String(config.laboratory.server.port) }));
  }, [config.laboratory.server]);

  const selectedWebAddress = status?.webAddresses.find((address) => address.address === selectedWebAddressKey) ?? status?.webAddresses[0] ?? null;

  useEffect(() => {
    const addresses = status?.webAddresses ?? [];
    setSelectedWebAddressKey((current) => current && addresses.some((address) => address.address === current) ? current : addresses[0]?.address ?? null);
  }, [status?.webAddresses]);

  useEffect(() => {
    if (!selectedWebAddress?.url) {
      setQrDataUrl(null);
      return;
    }
    let active = true;
    void QRCode.toDataURL(selectedWebAddress.url, { width: 220, margin: 1, errorCorrectionLevel: "M" })
      .then((dataUrl) => { if (active) setQrDataUrl(dataUrl); })
      .catch(() => { if (active) setQrDataUrl(null); });
    return () => { active = false; };
  }, [selectedWebAddress?.url]);

  useEffect(() => {
    if (status?.role !== "client" || scannedRef.current || status.running) return;
    scannedRef.current = true;
    void api.scanServers().then((records) => {
      setStatus((current) => current ? { ...current, recentServers: records } : current);
    }).catch(() => undefined);
  }, [status?.role, status?.running]);

  const run: LaboratoryRun = async (operation, notice) => {
    setBusy(true);
    setError(null);
    try {
      const next = await operation();
      if (next && typeof next === "object" && "phase" in next) setStatus(next as LaboratoryStatus);
      if (notice) setNotice(notice);
    } catch (error) {
      setError(messageOf(error));
    } finally {
      setBusy(false);
    }
  };

  const serverSettings = useMemo(() => ({
    ...serverDraft,
    port: Math.max(1024, Math.min(65535, Number(serverDraft.port) || config.laboratory.server.port)),
    debounceMs: Math.max(50, Math.min(10000, Number(serverDraft.debounceMs) || 1000)),
  }), [serverDraft, config.laboratory.server.port]);

  const selectDirection = (next: LaboratoryDirection) => {
    setSearchParams(next === "sync" ? {} : { direction: next }, { replace: true });
  };
  const saveServerSettings = () => void run(() => api.setServerSettings(serverSettings), t("settings.laboratory.saved"));
  const saveServerName = (name: string) => {
    const next = { ...serverSettings, name };
    setServerDraft((current) => ({ ...current, name }));
    void run(() => api.setServerSettings(next));
  };
  const setServerField = (field: "port" | "debounceMs", value: number) => setServerDraft((current) => ({ ...current, [field]: value }));
  const setDiscovery = (enabled: boolean) => {
    const next = { ...serverSettings, discoveryEnabled: enabled };
    setServerDraft((current) => ({ ...current, discoveryEnabled: enabled }));
    void run(() => api.setServerSettings(next));
  };
  const setWebEnabled = (enabled: boolean) => {
    const next = { ...serverSettings, webEnabled: enabled };
    setServerDraft((current) => ({ ...current, webEnabled: enabled }));
    void run(() => api.setServerSettings(next));
  };
  const copyWebUrl = async () => {
    if (!selectedWebAddress?.url) return;
    try {
      await navigator.clipboard.writeText(selectedWebAddress.url);
      setNotice(t("settings.laboratory.linkCopied"));
    } catch (error) {
      setError(messageOf(error));
    }
  };
  const connect = (record: LaboratoryServerRecord, enteredPassword = connectionPasswords[record.serverId] ?? "") => void run(
    () => api.connect({ serverId: record.serverId, name: record.name, address: record.address, port: record.port, requiresPassword: record.requiresPassword, webAvailable: record.webAvailable, password: enteredPassword }),
    t("settings.laboratory.connecting"),
  );
  const savePassword = () => void run(
    () => api.setServerPassword(password).then((next) => { setPassword(""); return next; }),
    t("settings.laboratory.passwordSaved"),
  );
  const submitManual = () => {
    const port = Number(manual.port);
    if (!manual.address.trim() || !Number.isInteger(port) || port < 1024 || port > 65535) {
      setError(t("settings.laboratory.manualInvalid"));
      return;
    }
    void run(() => api.connect({ serverId: null, name: manual.name.trim() || manual.address.trim(), address: manual.address.trim(), port, requiresPassword: Boolean(manual.password), webAvailable: false, password: manual.password }), t("settings.laboratory.connecting"));
  };

  const directionSections = direction === "hardware"
    ? [{ id: "laboratory-hardware", label: t("settings.laboratory.directions.hardware") }]
    : direction === "web"
      ? [
        { id: "laboratory-web", label: t("settings.laboratory.sections.web") },
        { id: "laboratory-themes", label: t("settings.laboratory.sections.themes") },
      ]
      : status?.role === "server"
        ? [
          { id: "laboratory-role", label: t("settings.laboratory.sections.role") },
          { id: "laboratory-server", label: t("settings.laboratory.sections.server") },
          { id: "laboratory-clients", label: t("settings.laboratory.clientsTitle") },
        ]
        : [
          { id: "laboratory-role", label: t("settings.laboratory.sections.role") },
          { id: "laboratory-client", label: t("settings.laboratory.sections.client") },
          { id: "laboratory-server-list", label: t("settings.laboratory.serverListTitle") },
        ];

  if (loading || !status) {
    return (
      <SettingsPage sections={[]}>
        <PageHeader title={t("settings.laboratory.title")} description={t("settings.laboratory.description")} />
        <DirectionSelector value={direction} onChange={selectDirection} t={t} />
        <div className={styles.laboratoryLoading}><Skeleton /><Skeleton /><Skeleton /></div>
      </SettingsPage>
    );
  }

  const isServer = status.role === "server";

  return (
    <SettingsPage sections={directionSections}>
      <PageHeader title={t("settings.laboratory.title")} description={t("settings.laboratory.description")} />
      <DirectionSelector value={direction} onChange={selectDirection} t={t} />

      {direction === "hardware" ? <HardwareDirection t={t} /> : direction === "web" ? (
        <>
          <WebServiceSection status={status} draft={serverDraft} busy={busy} isServer={isServer} qrDataUrl={qrDataUrl} selectedAddress={selectedWebAddress?.address ?? null} selectedWebAddress={selectedWebAddress} onAddressSelect={setSelectedWebAddressKey} onWebEnabledChange={setWebEnabled} copyWebUrl={() => void copyWebUrl()} resetWebToken={() => void run(() => api.resetWebToken(), t("settings.laboratory.tokenReset"))} t={t} />
          <ThemesSection status={status} busy={busy} onOpenDirectory={() => void api.revealThemesDirectory().catch((error) => setError(messageOf(error)))} t={t} />
        </>
      ) : (
        <>
          <RuntimeSection status={status} autoStart={config.laboratory.autoStart} busy={busy} run={run} t={t} />
          {isServer ? (
            <>
              <ServerSettingsSection
                title={t("settings.laboratory.serverSettingsTitle")}
                draft={serverDraft}
                status={status}
                password={password}
                busy={busy}
                showDiscovery
                showPassword
                onNameChange={saveServerName}
                onPortChange={(value) => setServerField("port", value)}
                onDebounceChange={(value) => setServerField("debounceMs", value)}
                onSave={saveServerSettings}
                onDiscoveryChange={setDiscovery}
                onPasswordChange={setPassword}
                onSavePassword={savePassword}
                t={t}
              />
              <ClientsSection status={status} busy={busy} run={run} t={t} />
            </>
          ) : (
            <>
              <ClientSettingsSection status={status} clientName={config.laboratory.client.name} busy={busy} manual={manual} setManual={setManual} run={run} submitManual={submitManual} t={t} />
              <RecentServersSection status={status} busy={busy} connectionPasswords={connectionPasswords} setConnectionPassword={(serverId, value) => setConnectionPasswords((current) => ({ ...current, [serverId]: value }))} connect={(record) => connect(record)} t={t} />
            </>
          )}
        </>
      )}
    </SettingsPage>
  );
}
