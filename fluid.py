"""
Lattice Boltzmann Method (D2Q9) fluid simulation with matplotlib inline rendering.
Styled for Gruvbox Dark.
"""

import matplotlib.colors as mcolors
import matplotlib.pyplot as plt
import numpy as np
from IPython.display import display

# ── Gruvbox Dark palette ──
GBX = {
    "bg0_h": "#1d2021",
    "bg": "#282828",
    "bg1": "#3c3836",
    "bg2": "#504945",
    "fg": "#ebdbb2",
    "fg4": "#a89984",
    "gray": "#928374",
    "red": "#fb4934",
    "green": "#b8bb26",
    "blue": "#83a598",
    "aqua": "#8ec07c",
    "orange": "#fe8019",
    "yellow": "#fabd2f",
    "purple": "#d3869b",
}


def gruvbox_diverging_cmap():
    """Blue → dark bg → red diverging colormap from Gruvbox palette."""
    colors = [GBX["blue"], GBX["bg0_h"], GBX["red"]]
    return mcolors.LinearSegmentedColormap.from_list("gruvbox_div", colors, N=256)


CMAP = gruvbox_diverging_cmap()

# ── Grid & physics params ──
NX, NY = 400, 100
TAU = 0.6  # relaxation time (viscosity ~ (tau - 0.5)/3)
STEPS = 3000
PLOT_EVERY = 30  # ~100 frames total, smooth for a demo
INLET_V = 0.04

# D2Q9 lattice velocities and weights
C = np.array(
    [[0, 0], [1, 0], [0, 1], [-1, 0], [0, -1], [1, 1], [-1, 1], [-1, -1], [1, -1]]
)
W = np.array([4 / 9, 1 / 9, 1 / 9, 1 / 9, 1 / 9, 1 / 36, 1 / 36, 1 / 36, 1 / 36])
NDIR = 9
OPP = np.array([0, 3, 4, 1, 2, 7, 8, 5, 6])

# Cylindrical obstacle
cx, cy, r = NX // 5, NY // 2, NY // 9
Y, X = np.meshgrid(np.arange(NY), np.arange(NX))
obstacle = (X - cx) ** 2 + (Y - cy) ** 2 < r**2


def equilibrium(rho, ux, uy):
    """Compute equilibrium distribution for all 9 directions."""
    feq = np.zeros((NDIR, NX, NY))
    usq = ux**2 + uy**2
    for i in range(NDIR):
        cu = C[i, 0] * ux + C[i, 1] * uy
        feq[i] = W[i] * rho * (1 + 3 * cu + 4.5 * cu**2 - 1.5 * usq)
    return feq


def style_ax(fig, ax, cbar):
    """Apply Gruvbox Dark styling to figure and axes."""
    fig.patch.set_alpha(0)
    ax.set_facecolor(GBX["bg0_h"])
    ax.patch.set_alpha(0.4)
    ax.tick_params(colors=GBX["fg4"], which="both")
    ax.xaxis.label.set_color(GBX["fg"])
    ax.yaxis.label.set_color(GBX["fg"])
    ax.title.set_color(GBX["fg"])
    for spine in ax.spines.values():
        spine.set_color(GBX["bg2"])
    cbar.ax.yaxis.set_tick_params(color=GBX["fg4"])
    cbar.outline.set_edgecolor(GBX["bg2"])
    cbar.ax.yaxis.label.set_color(GBX["fg"])
    plt.setp(cbar.ax.yaxis.get_ticklabels(), color=GBX["fg4"])


def main(rho: np.ndarray, ux: np.ndarray, uy: np.ndarray):
    f = equilibrium(rho, ux, uy)

    # ── Setup plot ──
    fig, ax = plt.subplots(figsize=(12, 3), dpi=120)
    norm = mcolors.Normalize(vmin=-0.05, vmax=0.05)
    im = ax.imshow(
        np.zeros((NX, NY)).T,
        cmap=CMAP,
        norm=norm,
        origin="lower",
        aspect="auto",
    )
    # Obstacle styled as Gruvbox bg1 with a subtle edge
    obs_mask = np.ma.masked_where(~obstacle, np.ones((NX, NY)))
    ax.imshow(
        obs_mask.T,
        cmap=mcolors.ListedColormap([GBX["bg1"]]),
        origin="lower",
        aspect="auto",
        alpha=0.95,
    )
    circle = plt.Circle((cx, cy), r, fill=False, edgecolor=GBX["gray"], linewidth=0.8)
    # Note: imshow axes are (col, row) so cx maps to x, cy to y
    ax.add_patch(circle)

    ax.set_title("Lattice Boltzmann — Vortex Shedding", fontweight="bold")
    ax.set_xlabel("x")
    ax.set_ylabel("y")
    cbar = fig.colorbar(im, ax=ax, label="vorticity (curl)", shrink=0.8, pad=0.02)
    style_ax(fig, ax, cbar)
    fig.tight_layout()
    plt.close(fig)

    h = display(fig, display_id=True)

    for step in range(STEPS):
        # ── Streaming ──
        for i in range(NDIR):
            f[i] = np.roll(f[i], C[i], axis=(0, 1))

        # ── Bounce-back on obstacle (from post-stream copy) ──
        f_obs = f[:, obstacle].copy()
        for i in range(NDIR):
            f[i][obstacle] = f_obs[OPP[i]]

        # ── Macroscopic quantities ──
        rho = np.sum(f, axis=0)
        ux = np.sum(f * C[:, 0, None, None], axis=0) / rho
        uy = np.sum(f * C[:, 1, None, None], axis=0) / rho

        # ── Inlet boundary ──
        ux[0, :] = INLET_V
        uy[0, :] = 0.0
        rho[0, :] = 1.0

        # Zero velocity inside obstacle
        ux[obstacle] = 0.0
        uy[obstacle] = 0.0

        # ── Collision (BGK) ──
        feq = equilibrium(rho, ux, uy)
        f += -(f - feq) / TAU

        # ── Render vorticity ──
        if step % PLOT_EVERY == 0 and step > 0:
            curl = (np.roll(uy, -1, axis=0) - np.roll(uy, 1, axis=0)) - (
                np.roll(ux, -1, axis=1) - np.roll(ux, 1, axis=1)
            )
            curl[obstacle] = np.nan
            im.set_data(curl.T)
            ax.set_title(
                f"Lattice Boltzmann — step {step}/{STEPS}",
                color=GBX["fg"],
                fontweight="bold",
            )
            h.update(fig)

    # ── Final frame ──
    curl = (np.roll(uy, -1, axis=0) - np.roll(uy, 1, axis=0)) - (
        np.roll(ux, -1, axis=1) - np.roll(ux, 1, axis=1)
    )
    curl[obstacle] = np.nan
    im.set_data(curl.T)
    ax.set_title(
        f"Lattice Boltzmann — step {STEPS}/{STEPS}",
        color=GBX["fg"],
        fontweight="bold",
    )
    h.update(fig)


# ── Init & run ──
rho = np.ones((NX, NY))
ux = np.full((NX, NY), INLET_V)
uy = np.zeros((NX, NY))

main(rho, ux, uy)
