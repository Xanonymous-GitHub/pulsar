# The one place where this tree changes what Buildroot does, instead of only configuring it.
#
# The machine has no floating-point unit (no F extension). Buildroot has a "soft float" setting
# for other architectures but not for RISC-V, so its uClibc-ng package turns on the code that
# talks to an FPU (`UCLIBC_HAS_FPU`, `UCLIBC_HAS_FENV`), which does not assemble for RV32IMA
# ("unrecognized opcode `frrm a0', extension `f' or `zfinx' required"). This replaces that part
# of the package's configuration; floating-point arithmetic is done in software by the compiler.
ifeq ($(BR2_riscv)$(BR2_RISCV_ISA_RVF),y)
define UCLIBC_FLOAT_CONFIG
	$(call KCONFIG_DISABLE_OPT,UCLIBC_HAS_FPU)
	$(call KCONFIG_DISABLE_OPT,UCLIBC_HAS_FENV)
	$(call KCONFIG_ENABLE_OPT,UCLIBC_HAS_FLOATS)
endef
endif
