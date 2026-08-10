// Package storagecontract defines response metadata shared by Storage providers
// and consumers. Keeping these values in one package prevents a Gateway 404
// from being confused with authoritative object-absence evidence.
package storagecontract

const (
	// ResultHeader is emitted by the Storage HEAD endpoint after the request has
	// reached the selected provider and the object lookup has completed.
	ResultHeader = "X-OJOS-Storage-Result"

	ResultPresent        = "present"
	ResultObjectNotFound = "object-not-found"
	ResultDeleted        = "deleted"
)
