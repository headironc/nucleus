import { operation, secret, type Platform } from "./platform";
import { Protocol, matchesLogout, validAccount, validSessions, type OwnSession, type Account, type Session, type LoginInput, type RegistrationInput } from "./protocol";
import { Storage, type Records, type Pending, type Selected } from "./storage";
import { Transport, requestTimeout, type Exchange, type Request } from "./transport";

export type View = {
  phase: "login" | "checking" | "registration" | "pending" | "account" | "sessions" | "unavailable" | "complete";
  sessions: OwnSession[];
  sessionsReady: boolean;
  homeAction: boolean;
  message: string;
  busy: boolean;
  canRecover: boolean;
  canLogin: boolean;
  canCheck: boolean;
  canReset: boolean;
  logoutPending: boolean;
  name: string;
  suffix: string;
  avatar: string;
  bound: boolean;
};
export type Selection = { generation: number; sessionId: string };
export type SessionTarget = Selection & { targetId: string };
const blank = (): View => ({ phase: "login", message: "使用微信绑定的手机号登录。首次使用可在验证后注册。", busy: false,
  sessions: [], sessionsReady: false, homeAction: false,
  canRecover: false, canLogin: true, canCheck: false, canReset: false, logoutPending: false, name: "", suffix: "", avatar: "", bound: false });

/** One identity owner for the visible native private page. No private projection is persisted. */
export class Controller {
  private storage: Storage;
  private transport: Transport;
  private records: Records = { version: 1 };
  private generation = 0;
  private visible = false;
  private page: "account" | "sessions" = "account";
  private otherUnknown?: string;
  private foreground = true;
  private active?: { exchange: Exchange; request: Request };
  private expiry?: ReturnType<typeof setTimeout>;
  private cleanupRetryAt?: number;
  private listener: (view: View) => void = () => {};
  private view: View = blank();
  constructor(private platform: Platform, origin: string, private protocol = new Protocol(), private now = Date.now) {
    this.storage = new Storage(platform, protocol);
    this.transport = new Transport(platform, origin);
  }
  subscribe(listener: (view: View) => void) { this.listener = listener; listener(this.snapshot()); }
  snapshot(): View { return { ...this.view, sessions: this.view.sessions.map(item => ({ ...item })) }; }
  private publish(patch: Partial<View>) {
    this.view = { ...this.view, ...patch };
    this.listener(this.snapshot());
  }
  private conceal() { this.publish({ name: "", suffix: "", avatar: "", bound: false, sessions: [], sessionsReady: false }); }
  private advance(): number {
    this.generation++;
    if (this.expiry) clearTimeout(this.expiry);
    this.expiry = undefined;
    this.conceal();
    const active = this.active;
    this.active = undefined;
    if (active?.exchange.pending) {
      if (active.request.method === "DELETE" && /^\/api\/v1\/sessions\/[0-9a-f-]+$/.test(active.request.path))
        this.otherUnknown = this.records.session?.id;
      this.preserveCancellation(active.request);
      active.exchange.cancel();
    }
    return this.generation;
  }
  private preserveCancellation(request: Request) {
    if (request.method !== "POST") return;
    const authentication = this.records.authentication;
    const registration = this.records.registration;
    const pending = request.path === "/api/v1/sessions" ? authentication :
      request.path === "/api/v1/registrations" ? registration?.pending : undefined;
    if (!pending || pending.operation !== request.operation) return;
    const interrupted = { ...pending, uncertain: true, retryAt: Math.max(pending.retryAt, this.now() + 5000) };
    this.records = request.path === "/api/v1/sessions" ? { ...this.records, authentication: interrupted as Pending<LoginInput> } :
      { ...this.records, registration: { ...registration!, pending: interrupted as Pending<RegistrationInput> } };
    try { this.storage.save(this.records); }
    catch { this.cleanupRetryAt = this.now() + 5000; }
  }
  hide() { this.visible = false; this.advance(); this.publish({ busy: false }); this.scheduleExpiry(); }
  suspend() { this.foreground = false; this.hide(); }
  resume() { this.foreground = true; this.refreshLocal(); }
  selection(): Selection | undefined {
    return this.visible && (this.view.phase === "account" || (this.view.phase === "sessions" && this.view.sessionsReady)) && this.records.session ?
      { generation: this.generation, sessionId: this.records.session.id } : undefined;
  }
  private matches(selection: Selection | undefined): boolean {
    return !!selection && this.current(selection.generation) &&
      (this.view.phase === "account" || (this.view.phase === "sessions" && this.view.sessionsReady)) && this.records.session?.id === selection.sessionId;
  }
  private current(generation: number): boolean { return this.visible && this.generation === generation; }
  private retain(next: Records) { this.storage.save(next); this.records = next; this.scheduleExpiry(); }
  private read() {
    const loaded = this.storage.load(this.now());
    const stored = JSON.stringify(loaded);
    // A failed cancellation save must not replace conservative local state with older bytes.
    function protect<T>(stored: Pending<T> | undefined, retained: Pending<T> | undefined) {
      return stored && retained && stored.operation === retained.operation &&
        stored.createdAt === retained.createdAt && stored.deadline === retained.deadline &&
        JSON.stringify(stored.input) === JSON.stringify(retained.input) ?
        { ...stored, uncertain: stored.uncertain || retained.uncertain, retryAt: Math.max(stored.retryAt, retained.retryAt) } : stored;
    }
    loaded.authentication = protect(loaded.authentication, this.records.authentication);
    if (loaded.registration) loaded.registration.pending = protect(loaded.registration.pending, this.records.registration?.pending);
    if (JSON.stringify(loaded) !== stored) this.storage.save(loaded);
    this.records = loaded;
    this.cleanupRetryAt = undefined;
    this.scheduleExpiry();
  }
  private scheduleExpiry() {
    if (this.expiry) clearTimeout(this.expiry);
    if (!this.foreground) return;
    const { session, authentication, registration } = this.records;
    const times = [session && Date.parse(session.expiresAt), authentication?.deadline,
      registration?.pending?.deadline ?? registration?.deadline, ...(this.records.logouts ?? []).map(value => value.deadline),
      authentication && authentication.retryAt > this.now() ? authentication.retryAt : undefined,
      registration?.pending && registration.pending.retryAt > this.now() ? registration.pending.retryAt : undefined,
    ].filter((value): value is number => typeof value === "number");
    if (!times.length && this.cleanupRetryAt === undefined) return;
    // JS timers accept a signed 32-bit delay; a 30-day session must not clamp to 1 ms.
    const deadline = this.cleanupRetryAt ?? Math.min(...times);
    this.expiry = setTimeout(() => { this.refreshLocal(); }, Math.max(1, Math.min(2147483647, deadline - this.now())));
  }
  private refreshLocal() {
    try {
      const before = this.records;
      this.read();
      if (!this.visible) return;
      if ((before.session && !this.records.session) || (before.authentication && !this.records.authentication) ||
          (before.registration && !this.records.registration)) {
        this.advance(); this.presentAnonymous(); this.scheduleExpiry();
      } else if (this.view.phase === "pending") {
        const material = this.records.authentication ?? this.records.registration?.pending;
        if (material) this.publish({ canRecover: !material.stopped && material.retryAt <= this.now() });
        else if (!this.selectedLogout() && this.records.session) {
          this.publish({ ...blank(), phase: "unavailable", canLogin: false, canCheck: true,
            message: "此前退出结果已无法继续查看。请检查当前登录状态。" });
        }
      } else if ((this.view.phase === "account" || this.view.phase === "sessions") && this.view.logoutPending && !this.selectedLogout()) {
        this.publish({ logoutPending: false, canRecover: false,
          message: "此前退出结果已无法继续查看，不表示此前退出失败。" });
      }
    } catch {
      if (this.visible) this.failedStorage();
      else this.retryLocalCleanup();
    }
  }
  private retryLocalCleanup() { this.cleanupRetryAt = this.now() + 5000; this.scheduleExpiry(); }
  private failedStorage() {
    this.advance();
    this.publish({ ...blank(), phase: "unavailable", canLogin: false, canCheck: true, canReset: true,
      message: "设备暂时无法读取或保存登录信息，请重新检查。" });
    this.retryLocalCleanup();
  }
  resetConfirmation(): number | undefined { return this.visible && this.view.canReset ? this.generation : undefined; }
  async restartLogin(generation: number) {
    if (!this.current(generation) || !this.view.canReset) return;
    this.advance();
    try {
      this.retain({ version: 1, unresolved: "authentication" });
      this.presentAnonymous();
    } catch { this.failedStorage(); }
  }
  async show(page: "account" | "sessions" = "account") {
    this.page = page;
    this.visible = true;
    const generation = this.advance();
    try {
      this.read();
      if (this.records.session) {
        if (this.selectedLogout()?.sessionId === this.records.session.id) {
          this.pending("暂时无法确认退出结果。可查看本次退出结果，或重新检查当前登录。", true);
        } else await this.checkSelected(generation);
      } else this.presentAnonymous();
    } catch { this.failedStorage(); }
  }
  private pending(message: string, canCheck = !!this.records.session) {
    const material = this.records.authentication ?? this.records.registration?.pending;
    this.publish({ ...blank(), phase: "pending", canLogin: false, canCheck,
      canRecover: material ? !material.stopped && material.retryAt <= this.now() : !!this.selectedLogout(),
      message });
  }
  private presentAnonymous() {
    if (this.records.authentication) {
      this.pending("登录状态待确认。请继续检查本次登录，暂时不用再次授权手机号。");
    } else if (this.records.registration?.pending) {
      this.pending("暂时无法确认注册结果。请继续检查本次注册，暂时不要再次提交。");
    } else if (this.records.registration) {
      this.publish({ ...blank(), phase: "registration", canLogin: false,
        message: "手机号已验证。请填写本人姓名与居民身份证号，提交后创建账户。" });
    } else {
      this.publish({ ...blank(), message: this.records.unresolved ?
        "此前办理结果仍无法确认。可重新验证手机号并检查当前账户；当前账户不证明此前注册结果。" : blank().message });
    }
  }
  async check() {
    const generation = this.advance();
    try {
      this.read();
      if (this.records.session) await this.checkSelected(generation);
      else this.presentAnonymous();
    } catch { this.failedStorage(); }
  }
  private async call(request: Request, generation: number) {
    if (!this.current(generation)) return undefined;
    const active = { exchange: this.transport.send(request), request };
    this.active = active;
    const reply = await active.exchange.promise;
    if (this.active === active) this.active = undefined;
    if (!this.current(generation)) return undefined;
    const path = request.path.split("?")[0].replace(/^(\/api\/v1\/sessions)\/[0-9a-f-]{36}$/, "$1/{id}");
    return this.protocol.decode(reply, `${request.method} ${path}` as Parameters<Protocol["decode"]>[1]);
  }
  private async checkSelected(generation: number) {
    const selected = this.records.session;
    if (!selected) return this.presentAnonymous();
    this.publish({ ...blank(), phase: "checking", busy: true, canLogin: false, message: "正在检查当前登录…" });
    const id = await operation(this.platform);
    if (!this.current(generation)) return;
    const result = await this.call({ method: "GET", path: "/api/v1/accounts/current", operation: id, token: selected.token }, generation);
    if (!this.current(generation)) return;
    if (Date.parse(selected.expiresAt) <= this.now()) return this.show(this.page);
    if (result?.status === 200) {
      const account = result.value.account as Account;
      if (validAccount(account) && (!selected.accountId || selected.accountId === account.id)) {
        this.retain({ ...this.records, session: { ...selected, accountId: account.id } });
        if (this.page === "sessions") return this.readSessions(generation);
        this.publish({ ...blank(), phase: "account", canLogin: false, canCheck: true,
          message: this.records.expiredLogouts?.includes(account.id) ? "此前退出结果已无法继续查看，不表示此前退出失败。" : "",
          name: account.name, suffix: account.idNumber.suffix, avatar: account.avatar.href,
          bound: account.binding !== null, logoutPending: !!this.selectedLogout(),
          canRecover: !!this.selectedLogout() });
        return;
      }
    }
    if (this.page === "sessions") return this.sessionFailure(result);
    if (result?.status === 401 && result.value.code === "authentication_invalid") {
      this.retain({ ...this.records, session: undefined });
      this.presentAnonymous();
      this.publish({ message: "请重新登录后继续。此前退出结果仍可在同一账户重新登录后查看。" });
    } else {
      this.publish({ ...blank(), phase: "unavailable", canLogin: false, canCheck: true,
        message: result?.status === 403 ? "你无权查看这项内容。" : "暂时无法加载，请刷新后重试。" });
    }
  }
  private async readSessions(generation: number) {
    const selected = this.records.session;
    if (!selected?.accountId) return;
    this.publish({ ...blank(), phase: "sessions", canLogin: false, busy: true, message: "正在加载登录列表…" });
    const id = await operation(this.platform);
    if (!this.current(generation)) return;
    const result = await this.call({ method: "GET", path: "/api/v1/sessions", operation: id, token: selected.token }, generation);
    if (!this.current(generation)) return;
    if (Date.parse(selected.expiresAt) <= this.now()) return this.show(this.page);
    if (result?.status === 200 && validSessions(result.value.sessions, selected.id)) {
      this.otherUnknown = undefined;
      this.publish({ ...blank(), phase: "sessions", canLogin: false, canCheck: true,
        sessions: result.value.sessions, sessionsReady: true, message: "列表已刷新。",
        logoutPending: !!this.selectedLogout(), canRecover: !!this.selectedLogout() });
    } else this.sessionFailure(result);
  }
  private sessionFailure(result: Awaited<ReturnType<Controller["call"]>>) {
    if (result?.status === 401 && result.value.code === "authentication_invalid") {
      this.retain({ ...this.records, session: undefined });
      this.publish({ ...blank(), message: "请重新登录。登录后可查看登录情况。" });
    } else {
      this.publish({ ...blank(), phase: "sessions", canLogin: false, canCheck: true,
        message: result?.status === 403 ? "目前无法查看登录情况。" :
          this.otherUnknown === this.records.session?.id ? "退出结果仍待确认，登录列表暂时无法加载。请刷新列表查看。" : "登录列表暂时无法加载，请重试。" });
    }
  }
  targetSelection(targetId: string): SessionTarget | undefined {
    const selected = this.selection();
    return selected && this.page === "sessions" && this.view.sessionsReady &&
      this.view.sessions.some(item => item.id === targetId && !item.current) ? { ...selected, targetId } : undefined;
  }
  async logoutOther(target: SessionTarget) {
    if (this.view.busy || !this.matches(target) || !this.targetSelection(target.targetId)) return;
    const sessions = this.view.sessions;
    const selected = this.records.session!;
    const generation = this.advance();
    this.publish({ ...blank(), phase: "sessions", canLogin: false, busy: true, message: "正在退出所选登录…" });
    try {
      this.read();
      if (this.records.session?.id !== selected.id) return this.show(this.page);
      const id = await operation(this.platform);
      if (!this.current(generation)) return;
      // This consumer refreshes current state after an unknown write; it never replays this DELETE.
      this.otherUnknown = selected.id;
      const result = await this.call({ method: "DELETE", path: `/api/v1/sessions/${target.targetId}`, operation: id, token: selected.token }, generation);
      if (!this.current(generation)) return;
      if (Date.parse(selected.expiresAt) <= this.now()) return this.show(this.page);
      const completion = result?.value.completion;
      if (result?.status === 200 && completion?.id === id && completion.count === 1) {
        this.otherUnknown = undefined;
        this.publish({ ...blank(), phase: "sessions", canLogin: false, canCheck: true, sessionsReady: true,
          sessions: sessions.filter(item => item.id !== target.targetId), message: "已退出所选登录。" });
      } else if (result?.status === 401 || result?.status === 403) this.sessionFailure(result);
      else this.publish({ ...blank(), phase: "sessions", canLogin: false, canCheck: true,
        message: result?.status === 404 && result.value.code === "resource_not_found" ? "所选登录暂时无法操作，请刷新列表。" :
          result?.status === 409 && result.value.code === "current_session" ? "所选登录是本次登录，不能在这里退出。请刷新列表。" :
          "退出结果待确认。暂时无法确认是否已退出，请刷新列表查看。" });
    } catch { if (this.current(generation)) this.failedStorage(); }
  }
  async authorize(code: unknown) {
    if (this.view.busy || !this.view.canLogin) return;
    if (typeof code !== "string" || !code) {
      this.publish({ message: "未取得手机号授权。你可以再次选择手机号登录。" });
      return;
    }
    const generation = this.advance();
    this.publish({ busy: true, canLogin: false, message: "正在验证手机号…" });
    try {
      this.read();
      if (this.records.authentication || this.records.registration || this.records.session) return this.presentAnonymous();
      const input: LoginInput = { secret: await secret(this.platform), credential: { type: "mobile", code } };
      const id = await operation(this.platform);
      if (!this.current(generation)) return;
      if (!this.protocol.validLogin(input)) throw new Error("Invalid login input");
      const now = this.now();
      this.retain({ ...this.records, unresolved: undefined,
        authentication: { operation: id, input, createdAt: now, deadline: now + 10 * 60000, retryAt: 0, stopped: false, uncertain: false } });
      await this.submitAuthentication(generation);
    } catch { if (this.current(generation)) this.writeFailed(!!this.records.authentication); }
  }
  private writeFailed(sent = true) {
    this.publish({ ...blank(), phase: "unavailable", canCheck: true, canLogin: false,
      message: sent ? "设备暂时无法保存办理结果，请重新检查。此前办理结果仍待确认。" :
        "设备暂时无法保存办理信息，本次操作尚未发送。" });
  }
  private async submitAuthentication(generation: number) {
    const pending = this.records.authentication;
    if (!pending || pending.stopped || this.now() < pending.retryAt || !this.inWindow(pending)) return this.presentAnonymous();
    const previouslyUnknown = pending.uncertain;
    // Cover a crash or failed cancellation save during the bounded native request wait.
    try {
      this.retain({ ...this.records, authentication: { ...pending, uncertain: true,
        retryAt: Math.max(pending.retryAt, this.now() + requestTimeout + 5000) } });
    } catch {
      if (this.current(generation)) this.writeFailed(previouslyUnknown);
      return;
    }
    const result = await this.call({ method: "POST", path: "/api/v1/sessions", operation: pending.operation, input: pending.input }, generation);
    if (!this.current(generation)) return;
    if (!this.inWindow(pending)) return this.show();
    if (result?.status === 200 && this.protocol.validSession(result.value.session)) {
      await this.acceptSession(result.value.session, generation);
    } else if (result?.status === 200 && this.protocol.validRegistration(result.value.registration)) {
      const proof = result.value.registration;
      const now = this.now();
      this.retain({ ...this.records, authentication: undefined,
        registration: { proof, createdAt: now, deadline: Math.min(now + 10 * 60000, Date.parse(proof.expiresAt)) } });
      this.presentAnonymous();
    } else if (["authentication_invalid", "permission_denied", "invalid_input"].includes(result?.value.code ?? "") && !previouslyUnknown) {
      this.retain({ ...this.records, authentication: undefined });
      this.presentAnonymous();
      this.publish({ message: result?.value.code === "permission_denied" ? "你无权进行此操作。" :
        result?.value.code === "authentication_invalid" ? "本次手机号验证未通过，请重新授权手机号。" : "请重新授权手机号后继续。" });
    } else if (result?.value.code === "verification_required" || result?.value.code === "mobile_unsupported") {
      this.retain({ ...this.records, authentication: undefined });
      this.presentAnonymous();
      this.publish({ message: result.value.code === "mobile_unsupported" ? "该手机号暂不支持。请使用中国大陆手机号。" : "本次验证已无法继续，请重新授权手机号。" });
    } else {
      const stopped = ["authentication_invalid", "request_mismatch", "permission_denied", "invalid_input"].includes(result?.value.code ?? "");
      this.retain({ ...this.records, authentication: { ...pending, stopped,
        uncertain: previouslyUnknown || !result || result.status >= 500,
        retryAt: this.now() + Math.max(5000, result?.retryAfter ?? 0) } });
      this.pending(stopped ? "暂时无法确认此前登录结果。请稍后重新检查，或返回公开首页。" : "登录状态待确认。请稍后继续检查本次登录，暂时不用再次授权手机号。");
    }
  }
  private inWindow(value: { createdAt: number; deadline: number }): boolean {
    return value.createdAt <= this.now() && this.now() < value.deadline;
  }
  async register(name: string, idNumber: string) {
    if (this.view.busy || this.view.phase !== "registration") return;
    const generation = this.advance();
    this.publish({ busy: true, message: "正在提交注册…" });
    try {
      this.read();
      const registration = this.records.registration;
      if (!registration || registration.pending || !this.inWindow(registration)) return this.presentAnonymous();
      const input: RegistrationInput = { id: registration.proof.id, token: registration.proof.token,
        name: name.trim().normalize("NFC"), idNumber: idNumber.trim().replace(/x$/, "X") };
      if (!this.protocol.validRegistrationInput(input) || !/^[0-9]{17}[0-9X]$/.test(input.idNumber)) {
        this.presentAnonymous();
        this.publish({ message: "请检查填写内容。姓名及 18 位居民身份证号均为必填。" });
        return;
      }
      const id = await operation(this.platform);
      if (!this.current(generation)) return;
      const now = this.now();
      this.retain({ ...this.records, registration: { ...registration,
        pending: { operation: id, input, createdAt: now, deadline: now + 10 * 60000, retryAt: 0, stopped: false, uncertain: false } } });
      await this.submitRegistration(generation);
    } catch { if (this.current(generation)) this.writeFailed(!!this.records.registration?.pending); }
  }
  private async submitRegistration(generation: number) {
    const registration = this.records.registration;
    const pending = registration?.pending;
    if (!registration || !pending || pending.stopped || this.now() < pending.retryAt || !this.inWindow(pending)) return this.presentAnonymous();
    const previouslyUnknown = pending.uncertain;
    try {
      this.retain({ ...this.records, registration: { ...registration, pending: { ...pending, uncertain: true,
        retryAt: Math.max(pending.retryAt, this.now() + requestTimeout + 5000) } } });
    } catch {
      if (this.current(generation)) this.writeFailed(previouslyUnknown);
      return;
    }
    const result = await this.call({ method: "POST", path: "/api/v1/registrations", operation: pending.operation, input: pending.input }, generation);
    if (!this.current(generation)) return;
    if (!this.inWindow(pending)) return this.show();
    if ((result?.status === 200 || result?.status === 201) && this.protocol.validSession(result.value.session)) {
      await this.acceptSession(result.value.session, generation);
    } else if (["mobile_taken", "id_number_taken"].includes(result?.value.code ?? "") ||
      (["invalid_input", "permission_denied"].includes(result?.value.code ?? "") && !previouslyUnknown)) {
      this.retain({ ...this.records, registration: result?.value.code === "mobile_taken" ? undefined : { ...registration, pending: undefined } });
      this.presentAnonymous();
      this.publish({ message: result?.value.code === "id_number_taken" ? "该身份证号已被使用。请检查填写内容。" :
        result?.value.code === "mobile_taken" ? "该手机号已被使用，请重新验证手机号后登录。" :
        result?.value.code === "permission_denied" ? "你无权进行此操作。" : "请检查填写内容。" });
    } else if (result?.value.code === "verification_required") {
      this.retain({ ...this.records, registration: undefined, unresolved: "registration" });
      this.presentAnonymous();
    } else {
      const stopped = ["authentication_invalid", "request_mismatch", "permission_denied", "invalid_input"].includes(result?.value.code ?? "");
      this.retain({ ...this.records, registration: { ...registration, pending: { ...pending, stopped,
        uncertain: previouslyUnknown || !result || result.status >= 500,
        retryAt: this.now() + Math.max(5000, result?.retryAfter ?? 0) } } });
      this.pending("暂时无法确认注册结果。请稍后检查本次注册，暂时不要再次提交。");
    }
  }
  private async acceptSession(session: Session, generation: number) {
    this.retain({ version: 1, session, ...(this.records.logouts ? { logouts: this.records.logouts } : {}),
      ...(this.records.expiredLogouts ? { expiredLogouts: this.records.expiredLogouts } : {}) });
    await this.checkSelected(generation);
  }
  async recover() {
    if (this.view.busy) return;
    const generation = this.advance();
    try {
      this.read();
      this.publish({ busy: true, canRecover: false, canLogin: false, message: "正在检查本次办理结果…" });
      if (this.records.authentication) await this.submitAuthentication(generation);
      else if (this.records.registration?.pending) await this.submitRegistration(generation);
      else if (this.selectedLogout()) await this.readLogout(generation);
      else if (this.records.session) await this.checkSelected(generation);
      else this.presentAnonymous();
    } catch { if (this.current(generation)) this.writeFailed(); }
  }
  async switchAccount(selection = this.selection()) {
    if (!this.matches(selection)) return;
    this.advance();
    try {
      this.read();
      this.retain({ ...this.records, session: undefined });
      this.presentAnonymous();
    } catch { this.failedStorage(); }
  }
  async logout(scope: "current" | "all", selection = this.selection()) {
    if (this.view.busy || !this.matches(selection)) return;
    const generation = this.advance();
    try {
      this.read();
      const selected = this.records.session;
      if (!selected?.accountId || this.selectedLogout()) return this.pending("请先查看此前退出结果。", true);
      this.publish({ busy: true, canLogin: false, message: "正在退出…" });
      const id = await operation(this.platform);
      if (!this.current(generation)) return;
      const now = this.now();
      this.retain({ ...this.records, logouts: [...(this.records.logouts ?? []), { operation: id, accountId: selected.accountId,
        sessionId: selected.id, scope, createdAt: now, deadline: now + 10 * 60000 }] });
      const path = scope === "current" ? "/api/v1/sessions/current" : "/api/v1/sessions";
      const result = await this.call({ method: "DELETE", path, operation: id, token: selected.token }, generation);
      if (!this.current(generation)) return;
      const completion = result?.value.completion;
      if (result?.status === 200 && completion?.id === id && Number.isSafeInteger(completion.count) &&
          completion.count > 0 && (scope === "all" || completion.count === 1)) {
        this.retain({ ...this.records, session: undefined, logouts: this.withoutSelectedLogout() });
        this.publish({ ...blank(), phase: "complete", homeAction: true, message: scope === "current" ? "已退出本次登录。" : "已退出全部登录。" });
      } else {
        if (result?.status === 401 && result.value.code === "authentication_invalid") {
          this.retain({ ...this.records, session: undefined });
          this.presentAnonymous();
          this.publish({ message: "暂时无法确认退出结果。请重新登录同一账户后查看本次退出结果。" });
        } else this.pending("暂时无法确认退出结果。请查看本次退出结果，或重新检查当前登录。", true);
      }
    } catch { if (this.current(generation)) this.writeFailed(); }
  }
  private async readLogout(generation: number) {
    const logout = this.selectedLogout();
    const selected = this.records.session;
    if (!logout || !selected?.accountId || logout.accountId !== selected.accountId) {
      this.pending("请先重新登录此前退出的同一账户，再查看本次退出结果。", !!selected);
      return;
    }
    const id = await operation(this.platform);
    if (!this.current(generation)) return;
    const result = await this.call({ method: "GET", path: `/api/v1/revocations?operation_id=${logout.operation}`,
      operation: id, token: selected.token }, generation);
    if (!this.current(generation)) return;
    if (result?.status === 200 && matchesLogout(result.value as any, logout.scope, logout.sessionId)) {
      this.retain({ ...this.records, logouts: this.withoutSelectedLogout(),
        session: selected.id === logout.sessionId ? undefined : selected });
      this.publish({ ...blank(), phase: "complete", canLogin: !this.records.session, canCheck: !!this.records.session,
        message: logout.scope === "current" ? "此前本次登录已退出。" : "此前全部登录退出已完成。后来建立的登录不受影响。" });
    } else if (result?.status === 401 && result.value.code === "authentication_invalid") {
      this.retain({ ...this.records, session: undefined });
      this.presentAnonymous();
      this.publish({ message: "请重新登录同一账户后查看此前退出结果。" });
    } else this.pending("暂时无法确认此前退出结果。当前没有可确认的退出证明，请稍后重试。", true);
  }
  private selectedLogout() {
    return this.records.logouts?.find(value => value.accountId === this.records.session?.accountId);
  }
  private withoutSelectedLogout() {
    return this.records.logouts?.filter(value => value.accountId !== this.records.session?.accountId);
  }
}
