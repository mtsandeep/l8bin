#!/usr/bin/env bash

set -e

TTY="/dev/tty"
if [ ! -e "$TTY" ]; then
  echo "No TTY detected. Run in an interactive shell."
  exit 1
fi

echo "======================================"
echo "   L8BIN VPS BOOTSTRAP (INTERACTIVE)  "
echo "======================================"
echo ""
echo "NOTE: this script only ADDS or OVERWRITES settings."
echo "Answering 'no' / skipping never disables or removes"
echo "anything already on the box."

# -----------------------------
# Helpers
# -----------------------------
ask() {
  local prompt="$1"
  local ans
  read -rp "$prompt " ans < "$TTY"
  echo "$ans"
}

confirm_yes() {
  local prompt="$1"
  local default_yes="$2"
  local ans

  if [ "$default_yes" = "yes" ]; then
    read -rp "$prompt (Y/n): " ans < "$TTY"
    [[ -z "$ans" || "$ans" == "y" || "$ans" == "Y" ]]
  else
    read -rp "$prompt (y/N): " ans < "$TTY"
    [[ "$ans" == "y" || "$ans" == "Y" ]]
  fi
}

trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  echo "$s"
}

# Space-separated unique usernames for docker group
DOCKER_USERS=""
append_docker_user() {
  local u="$1"
  [ -z "$u" ] && return
  [ "$u" = "root" ] && return
  case " $DOCKER_USERS " in
    *" $u "*) ;;
    *) DOCKER_USERS="${DOCKER_USERS:+$DOCKER_USERS }$u" ;;
  esac
}

user_home() {
  getent passwd "$1" | cut -d: -f6
}

has_authorized_keys() {
  local user="$1"
  local home keys
  home=$(user_home "$user")
  keys="${home}/.ssh/authorized_keys"
  [ -n "$home" ] && [ -f "$keys" ] && [ -s "$keys" ]
}

install_authorized_keys_file() {
  local user="$1"
  local src_file="$2"
  local home
  home=$(user_home "$user")
  [ -n "$home" ] || return 1

  mkdir -p "${home}/.ssh"
  chmod 700 "${home}/.ssh"
  cat "$src_file" >> "${home}/.ssh/authorized_keys"
  sort -u "${home}/.ssh/authorized_keys" -o "${home}/.ssh/authorized_keys"
  chmod 600 "${home}/.ssh/authorized_keys"
  chown -R "${user}:${user}" "${home}/.ssh"
}

install_authorized_keys_line() {
  local user="$1"
  local pubkey="$2"
  local tmp
  tmp="$(mktemp)"
  printf '%s\n' "$pubkey" > "$tmp"
  install_authorized_keys_file "$user" "$tmp"
  rm -f "$tmp"
}

prompt_paste_pubkey() {
  local for_user="$1"
  echo ""
  echo "Generate a keypair on your LOCAL machine (not this server), then come back:"
  echo ""
  echo "  ssh-keygen -t ed25519 -a 100 -C \"${for_user}@$(hostname -s 2>/dev/null || echo vps)\""
  echo ""
  echo "That creates ~/.ssh/id_ed25519 (private — keep secret) and"
  echo "             ~/.ssh/id_ed25519.pub (public — paste below)."
  echo ""
  echo "Show the public key with:"
  echo "  cat ~/.ssh/id_ed25519.pub"
  echo ""
  if ! confirm_yes "Ready to paste the public key now?" "yes"; then
    SSH_KEY_MODE="none"
    SSH_PUBKEY=""
    echo "OK — you can set a login password instead so SSH still works."
    return
  fi
  echo -n "Paste the single public key line, then Enter: "
  read -r SSH_PUBKEY < "$TTY"
  SSH_PUBKEY="$(trim "$SSH_PUBKEY")"
  if [ -z "$SSH_PUBKEY" ]; then
    echo "Empty key — not saved."
    SSH_KEY_MODE="none"
    return
  fi
  if [[ ! "$SSH_PUBKEY" =~ ^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256|sk-ssh-ed25519@openssh\.com|sk-ecdsa-sha2-nistp256@openssh\.com)[[:space:]] ]]; then
    echo "WARNING: that does not look like a public key line (expected ssh-ed25519 AAAA...)."
    if ! confirm_yes "Use it anyway?" "no"; then
      SSH_KEY_MODE="none"
      SSH_PUBKEY=""
      return
    fi
  fi
  SSH_KEY_MODE="paste"
}

ensure_login_method() {
  if [ "$SSH_KEY_MODE" = "none" ]; then
    echo ""
    echo "No SSH key configured for '$USERNAME'."
    echo "A password is required so this user can still log in over SSH."
    if confirm_yes "Set a login password for '$USERNAME' now?" "yes"; then
      SSH_KEY_MODE="password"
    else
      echo "WARNING: '$USERNAME' will have neither an SSH key nor a password."
    fi
  fi
}

INVOKER=""
if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
  INVOKER="$SUDO_USER"
elif [ "$(id -u)" -ne 0 ]; then
  INVOKER="$(id -un)"
fi

# -----------------------------
# 1. SSH CONFIG
# -----------------------------
echo ""
echo "---- SSH Configuration ----"

if confirm_yes "Allow root login via SSH?" "yes"; then
  ALLOW_ROOT=true
else
  ALLOW_ROOT=false
fi

SSH_PORT=$(ask "SSH Port (default 22):")
SSH_PORT=${SSH_PORT:-22}

# -----------------------------
# 2. SUDO ADMIN USER + LOGIN METHOD
# -----------------------------
echo ""
echo "---- Admin user ----"
echo "A sudo admin is a normal Linux user in the 'sudo' group."
echo "They are not root, but can run privileged commands with sudo."

CREATE_USER=false
USERNAME=""
SSH_KEY_MODE="none"   # none | copy_invoker | paste | password
SSH_PUBKEY=""

if confirm_yes "Create a sudo admin user?" "no"; then
  CREATE_USER=true
  USERNAME=$(ask "Enter username:")
  USERNAME="$(trim "$USERNAME")"
  if [ -z "$USERNAME" ]; then
    echo "No username given — skipping user creation."
    CREATE_USER=false
  fi
fi

if $CREATE_USER; then
  echo ""
  echo "---- Login method for '$USERNAME' ----"
  echo "Pick how this user will sign in over SSH (key and/or password)."
  echo ""
  if [ -n "$INVOKER" ]; then
    echo "  1) Copy SSH keys from '$INVOKER' (current user running this script)"
  else
    echo "  1) Copy SSH keys from current user (not available — script is running as root)"
  fi
  echo "  2) Add a new SSH key (generate locally, paste the public key)"
  echo "  3) Set a login password for '$USERNAME'"
  echo ""

  SSH_KEY_CHOICE=$(ask "Choice [1-3]:")
  case "$SSH_KEY_CHOICE" in
    1)
      if [ -n "$INVOKER" ] && has_authorized_keys "$INVOKER"; then
        SSH_KEY_MODE="copy_invoker"
      elif [ -z "$INVOKER" ]; then
        echo "No current non-root user detected."
        prompt_paste_pubkey "$USERNAME"
        ensure_login_method
      else
        echo "No authorized_keys found for '$INVOKER'."
        prompt_paste_pubkey "$USERNAME"
        ensure_login_method
      fi
      ;;
    2)
      prompt_paste_pubkey "$USERNAME"
      ensure_login_method
      ;;
    3)
      SSH_KEY_MODE="password"
      ;;
    *)
      echo "Invalid choice — defaulting to set a login password."
      SSH_KEY_MODE="password"
      ;;
  esac
fi

# -----------------------------
# 3. SSH HARDENING
# -----------------------------
echo ""
echo "---- SSH hardening ----"

if confirm_yes "Disable password login (SSH key-only)?" "no"; then
  USE_KEY_ONLY=true
else
  USE_KEY_ONLY=false
fi

if [ "$USE_KEY_ONLY" = true ] && [ "$SSH_KEY_MODE" = "password" ]; then
  echo "WARNING: '$USERNAME' is set up with a password, not an SSH key."
  echo "Key-only SSH would lock that user out."
  USE_KEY_ONLY=false
  echo "Leaving password authentication enabled."
fi

if [ "$ALLOW_ROOT" = false ] && [ "$CREATE_USER" = false ]; then
  echo "WARNING: Root SSH will be disabled and no sudo admin user will be created."
  echo "Make sure another existing user can already SSH in (e.g. ubuntu on Oracle Cloud)."
  if ! confirm_yes "Continue with root login disabled?" "no"; then
    ALLOW_ROOT=true
    echo "Keeping root login enabled."
  fi
fi

if [ "$ALLOW_ROOT" = false ] && [ "$CREATE_USER" = true ] && [ "$SSH_KEY_MODE" = "none" ]; then
  echo "WARNING: Root SSH disabled, and '$USERNAME' has neither an SSH key nor a password."
  echo "You may lock yourself out."
  if ! confirm_yes "Continue anyway?" "no"; then
    ALLOW_ROOT=true
    echo "Keeping root login enabled."
  fi
fi

if [ "$USE_KEY_ONLY" = true ]; then
  if [ "$CREATE_USER" = true ] && [ "$SSH_KEY_MODE" != "copy_invoker" ] && [ "$SSH_KEY_MODE" != "paste" ]; then
    echo "WARNING: Key-only SSH requires an SSH key for '$USERNAME'."
    if ! confirm_yes "Continue with key-only SSH anyway?" "no"; then
      USE_KEY_ONLY=false
      echo "Leaving password authentication enabled."
    fi
  fi
fi

# -----------------------------
# 4. FIREWALL
# -----------------------------
echo ""
echo "---- Firewall (UFW) ----"

if confirm_yes "Configure firewall (UFW) now? (no = skip, keeps current)" "yes"; then
  ENABLE_UFW=true
  EXTRA_PORTS=$(ask "Extra ports (comma-separated, optional):")
else
  ENABLE_UFW=false
  EXTRA_PORTS=""
fi

# -----------------------------
# 5. FAIL2BAN
# -----------------------------
echo ""
echo "---- Fail2Ban ----"

if confirm_yes "Install/configure Fail2Ban? (no = skip, keeps current)" "yes"; then
  ENABLE_F2B=true
else
  ENABLE_F2B=false
fi

# -----------------------------
# 6. AUTO UPDATES
# -----------------------------
echo ""
echo "---- Auto Security Updates ----"

if confirm_yes "Configure unattended security updates? (no = skip, keeps current)" "yes"; then
  ENABLE_AUTO_UPDATES=true
else
  ENABLE_AUTO_UPDATES=false
fi

# -----------------------------
# 7. DOCKER
# -----------------------------
echo ""
echo "---- Docker ----"

if command -v docker >/dev/null 2>&1; then
  echo "Docker already installed."
  INSTALL_DOCKER=false
else
  if confirm_yes "Docker not found. Install Docker?" "yes"; then
    INSTALL_DOCKER=true
  else
    INSTALL_DOCKER=false
  fi
fi

DOCKER_USERS=""
if [ "$INSTALL_DOCKER" = true ] || command -v docker >/dev/null 2>&1; then
  echo ""
  echo "---- Docker group ----"
  echo "Users in the docker group can run docker without sudo."
  echo "root can already use Docker; it is not added to the group."
  echo ""

  DOCKER_USERS_DEFAULT=""
  if [ -n "$INVOKER" ]; then
    DOCKER_USERS_DEFAULT="$INVOKER"
  fi
  if [ "$CREATE_USER" = true ] && [ -n "$USERNAME" ] && [ "$USERNAME" != "$INVOKER" ]; then
    DOCKER_USERS_DEFAULT="${DOCKER_USERS_DEFAULT:+$DOCKER_USERS_DEFAULT,}$USERNAME"
  fi

  if [ -n "$DOCKER_USERS_DEFAULT" ]; then
    echo "  1) ${DOCKER_USERS_DEFAULT} (recommended)"
    echo "  2) Custom list (type usernames)"
    echo "  3) Skip (add no one)"
    echo ""
    DOCKER_CHOICE=$(ask "Choice [1]:")
    DOCKER_CHOICE=${DOCKER_CHOICE:-1}
  else
    echo "  1) Custom list (type usernames)"
    echo "  2) Skip (add no one)"
    echo ""
    DOCKER_CHOICE=$(ask "Choice [2]:")
    DOCKER_CHOICE=${DOCKER_CHOICE:-2}
    if [ "$DOCKER_CHOICE" = "1" ]; then
      DOCKER_CHOICE="2"
    elif [ "$DOCKER_CHOICE" = "2" ]; then
      DOCKER_CHOICE="3"
    fi
  fi

  DOCKER_USERS=""
  case "$DOCKER_CHOICE" in
    1)
      IFS=',' read -ra DOCKER_ARR <<< "$DOCKER_USERS_DEFAULT"
      for u in "${DOCKER_ARR[@]}"; do
        u="$(trim "$u")"
        append_docker_user "$u"
      done
      ;;
    2)
      DOCKER_USERS_INPUT=$(ask "Usernames (comma-separated):")
      if [ -n "$DOCKER_USERS_INPUT" ]; then
        IFS=',' read -ra DOCKER_ARR <<< "$DOCKER_USERS_INPUT"
        for u in "${DOCKER_ARR[@]}"; do
          u="$(trim "$u")"
          append_docker_user "$u"
        done
      fi
      ;;
    *)
      ;;
  esac

  if [ -n "$DOCKER_USERS" ]; then
    echo "Will add to docker group: $DOCKER_USERS"
  else
    echo "No users will be added to docker group."
  fi
fi

# -----------------------------
# 8. CLEANUP
# -----------------------------
echo ""
echo "---- Cleanup (apt packages only) ----"

if confirm_yes "Install weekly apt-cleanup cron (autoremove + autoclean)? (no = skip, keeps current)" "yes"; then
  ENABLE_CLEANUP=true
else
  ENABLE_CLEANUP=false
fi

# -----------------------------
# SUMMARY
# -----------------------------
echo ""
echo "----------- SUMMARY -----------"
echo "SSH Port: $SSH_PORT"
echo "Root Login: $ALLOW_ROOT"
echo "Password Auth Disabled: $USE_KEY_ONLY"
echo "Create sudo admin: $CREATE_USER ${USERNAME:+($USERNAME)}"
echo "Login method for new user: $SSH_KEY_MODE"
if $CREATE_USER && { [ "$SSH_KEY_MODE" = "copy_invoker" ] || [ "$SSH_KEY_MODE" = "paste" ]; }; then
  echo "Passwordless sudo: yes (key-only account → NOPASSWD granted)"
else
  echo "Passwordless sudo: no"
fi
echo "Firewall: $ENABLE_UFW"
echo "Extra Ports: ${EXTRA_PORTS:-none}"
echo "Fail2Ban: $ENABLE_F2B"
echo "Auto Updates: $ENABLE_AUTO_UPDATES"
echo "Install Docker: $INSTALL_DOCKER"
echo "Docker group users: ${DOCKER_USERS:-none}"
echo "apt cleanup: $ENABLE_CLEANUP"
echo "--------------------------------"

if ! confirm_yes "Proceed with setup?" "yes"; then
  echo "Aborted."
  exit 0
fi

# =============================
# EXECUTION
# =============================

echo ""
echo "Applying configuration..."

# Use a drop-in so settings win over cloud image defaults in sshd_config.d/
SSH_DROPIN="/etc/ssh/sshd_config.d/99-l8bin.conf"
SSH_PORT_APPLIED=""
SSH_ROOT_APPLIED=""
SSH_PASS_APPLIED=""

write_ssh_dropin() {
  local port="$1"
  local permit_root="$2"
  local password_auth="$3"
  mkdir -p /etc/ssh/sshd_config.d
  cat > "$SSH_DROPIN" <<EOF
# Managed by l8bin vps-setup.sh
Port ${port}
PermitRootLogin ${permit_root}
PasswordAuthentication ${password_auth}
PubkeyAuthentication yes
EOF
  SSH_PORT_APPLIED="$port"
  SSH_ROOT_APPLIED="$permit_root"
  SSH_PASS_APPLIED="$password_auth"
}

restart_ssh() {
  if ! sshd -t 2>/dev/null; then
    echo "Warning: sshd config test failed — not restarting SSH."
    sshd -t || true
    return 1
  fi
  systemctl restart ssh 2>/dev/null || systemctl restart sshd 2>/dev/null || \
    echo "Warning: could not restart ssh/sshd — restart it manually."
}

# 1) Create user + keys/password (no apt yet — so verify is not delayed)
if $CREATE_USER; then
  if ! id "$USERNAME" &>/dev/null; then
    adduser --disabled-password --gecos "" "$USERNAME"
    echo "Created user '$USERNAME'."
  else
    echo "User '$USERNAME' already exists."
  fi
  usermod -aG sudo "$USERNAME"
  echo "Ensured '$USERNAME' is in the sudo group."

  # Sudoers: a key-only account (no Linux password) cannot authenticate to
  # sudo — the default %sudo rule requires a password that doesn't exist.
  # That locks the user out of root via sudo. Fix it by granting NOPASSWD
  # to key-only admins. The SSH key remains the auth boundary; this just
  # lets sudo work inside that boundary.
  SUDOERS_FILE="/etc/sudoers.d/${USERNAME}"
  if [ "$SSH_KEY_MODE" = "copy_invoker" ] || [ "$SSH_KEY_MODE" = "paste" ]; then
    echo "${USERNAME} ALL=(ALL) NOPASSWD:ALL" > "$SUDOERS_FILE"
    chmod 440 "$SUDOERS_FILE"
    # visudo -c validates syntax; abort cleanly if broken (never leave a
    # bad sudoers file on disk — it can block all sudo).
    if visudo -cf "$SUDOERS_FILE" >/dev/null 2>&1; then
      echo "Granted passwordless sudo to '$USERNAME' (key-only account)."
    else
      echo "WARNING: sudoers file for '$USERNAME' failed validation — removing it."
      rm -f "$SUDOERS_FILE"
      echo "You will need to set a password for sudo: passwd $USERNAME"
    fi
  fi

  case "$SSH_KEY_MODE" in
    copy_invoker)
      install_authorized_keys_file "$USERNAME" "$(user_home "$INVOKER")/.ssh/authorized_keys"
      echo "Copied authorized_keys from '$INVOKER' → '$USERNAME'."
      ;;
    paste)
      install_authorized_keys_line "$USERNAME" "$SSH_PUBKEY"
      echo "Installed pasted public key for '$USERNAME'."
      ;;
    password)
      echo "Set a password for '$USERNAME' (you will be prompted twice):"
      if passwd "$USERNAME"; then
        echo "Password set for '$USERNAME'."
      else
        echo "WARNING: password was not set for '$USERNAME'."
        SSH_KEY_MODE="none"
      fi
      ;;
    none)
      echo "No SSH key or password configured for '$USERNAME'."
      echo "Add a key under $(user_home "$USERNAME")/.ssh/authorized_keys or run: passwd $USERNAME"
      ;;
  esac
fi

# Refresh package indexes before any apt install
apt update -y

# 2) Open SSH port in firewall BEFORE changing sshd port (avoids lockout)
if $ENABLE_UFW; then
  if ! apt install -y ufw; then
    echo "WARNING: could not install ufw — skipping firewall setup."
    ENABLE_UFW=false
  else
    ufw allow "$SSH_PORT"/tcp
  fi
fi

# 3) Apply SSH port now; leave root/password usable until after verify
write_ssh_dropin "$SSH_PORT" "yes" "yes"
restart_ssh || true

# 4) Verify only when we created a login or changed the SSH port
NEED_VERIFY=false
VERIFY_USER="${INVOKER:-ubuntu}"
VERIFY_HOW="your usual login method"

if $CREATE_USER; then
  case "$SSH_KEY_MODE" in
    copy_invoker|paste)
      NEED_VERIFY=true
      VERIFY_USER="$USERNAME"
      VERIFY_HOW="your SSH key"
      ;;
    password)
      NEED_VERIFY=true
      VERIFY_USER="$USERNAME"
      VERIFY_HOW="the password you set"
      ;;
  esac
fi

if [ "$SSH_PORT" != "22" ]; then
  NEED_VERIFY=true
fi

if $NEED_VERIFY && { [ "$USE_KEY_ONLY" = true ] || [ "$ALLOW_ROOT" = false ] || [ "$SSH_PORT" != "22" ]; }; then
  echo ""
  echo "---- Verify SSH access ----"
  echo "Root/password lockdown has not been applied yet."
  if [ "$SSH_PORT" != "22" ]; then
    echo "SSH port is now ${SSH_PORT}. From a NEW terminal try:"
    echo "  ssh -p ${SSH_PORT} ${VERIFY_USER}@<server-ip>"
  else
    echo "From a NEW terminal try:"
    echo "  ssh ${VERIFY_USER}@<server-ip>"
  fi
  echo "Log in with ${VERIFY_HOW}."
  echo "Keep THIS session open until that works."
  if ! confirm_yes "Did that login work?" "no"; then
    echo "Skipping SSH lockdown (keeping root/password settings unchanged)."
    ALLOW_ROOT=true
    USE_KEY_ONLY=false
  fi
fi

# 5) Apply chosen root / password lockdown
FINAL_ROOT="yes"
FINAL_PASS="yes"
[ "$ALLOW_ROOT" = true ] || FINAL_ROOT="no"
if [ "$USE_KEY_ONLY" = true ]; then
  FINAL_PASS="no"
fi
write_ssh_dropin "$SSH_PORT" "$FINAL_ROOT" "$FINAL_PASS"
restart_ssh || true
echo "SSH applied: Port=${SSH_PORT_APPLIED}, PermitRootLogin=${SSH_ROOT_APPLIED}, PasswordAuthentication=${SSH_PASS_APPLIED}"

# 6) Rest of system setup

# UFW
if $ENABLE_UFW; then
  ufw default deny incoming
  ufw default allow outgoing
  ufw allow "$SSH_PORT"/tcp

  if [ -n "$EXTRA_PORTS" ]; then
    IFS=',' read -ra PORTS <<< "$EXTRA_PORTS"
    for p in "${PORTS[@]}"; do
      p="$(trim "$p")"
      [ -n "$p" ] && ufw allow "$p"/tcp
    done
  fi

  ufw --force enable
fi

# FAIL2BAN
if $ENABLE_F2B; then
  apt install -y fail2ban
  cat > /etc/fail2ban/jail.local <<EOF
[sshd]
enabled = true
port = $SSH_PORT
EOF
  systemctl enable fail2ban
  systemctl restart fail2ban
fi

# AUTO UPDATES
if $ENABLE_AUTO_UPDATES; then
  apt install -y unattended-upgrades
  dpkg-reconfigure -f noninteractive unattended-upgrades
fi

# DOCKER
if $INSTALL_DOCKER; then
  apt install -y ca-certificates curl gnupg
  install -m 0755 -d /etc/apt/keyrings

  curl -fsSL https://download.docker.com/linux/$(. /etc/os-release && echo "$ID")/gpg \
    | gpg --dearmor -o /etc/apt/keyrings/docker.gpg

  chmod a+r /etc/apt/keyrings/docker.gpg

  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
  https://download.docker.com/linux/$(. /etc/os-release && echo "$ID") \
  $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
  > /etc/apt/sources.list.d/docker.list

  apt update -y
  apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

  systemctl enable docker
  systemctl start docker
fi

# DOCKER GROUP
if [ -n "$DOCKER_USERS" ]; then
  echo ""
  echo "---- Docker group ----"
  if ! getent group docker >/dev/null 2>&1; then
    echo "docker group not found — install Docker first, then re-run this step."
  else
    for u in $DOCKER_USERS; do
      if ! id "$u" &>/dev/null; then
        echo "User '$u' does not exist — skipping."
        continue
      fi
      if id -nG "$u" | grep -qw docker; then
        echo "User '$u' already in docker group."
        continue
      fi
      usermod -aG docker "$u"
      echo "Added '$u' to docker group."
    done
    echo "Note: those users must log out and back in (or run: newgrp docker) before docker works without sudo."
  fi
fi

# CLEANUP
if $ENABLE_CLEANUP; then
  echo "0 3 * * 0 root apt autoremove -y && apt autoclean -y" > /etc/cron.d/l8bin-cleanup
fi

echo ""
echo "======================================"
echo "   Setup Complete"
echo "======================================"
echo ""